//! Pipelined extraction into a filesystem directory.

mod path_splitting {
    use displaydoc::Display;
    use thiserror::Error;

    use std::collections::BTreeMap;

    use crate::spec::is_dir;

    /// Errors encountered during path splitting.
    #[derive(Debug, Display, Error)]
    pub enum PathSplitError {
        /// entry path format error: {0}
        PathFormat(String),
        /// file and directory paths overlapped: {0}
        FileDirOverlap(String),
    }

    fn split_by_separator<'a>(
        entry_path: &'a str,
    ) -> Result<impl Iterator<Item = &'a str>, PathSplitError> {
        if entry_path.contains('\\') {
            if entry_path.contains('/') {
                return Err(PathSplitError::PathFormat(format!(
                    "path {:?} contained both '\\' and '/' separators",
                    entry_path
                )));
            }
            Ok(entry_path.split('\\'))
        } else {
            Ok(entry_path.split('/'))
        }
    }

    /* TODO: consider using crate::unstable::path_to_string() for this--it involves new
     * allocations, but that really shouldn't matter for our purposes. I like the idea of using our
     * own logic here, since parallel/pipelined extraction is really a different use case than the
     * rest of the zip crate, but it's definitely worth considering. */
    fn normalize_parent_dirs<'a>(
        entry_path: &'a str,
    ) -> Result<(Vec<&'a str>, bool), PathSplitError> {
        if entry_path.starts_with('/') || entry_path.starts_with('\\') {
            return Err(PathSplitError::PathFormat(format!(
                "path {:?} began with '/' or '\\' and is absolute",
                entry_path
            )));
        }
        let is_dir = is_dir(entry_path);

        let mut ret: Vec<&'a str> = Vec::new();
        for component in split_by_separator(entry_path)? {
            match component {
                /* Skip over repeated separators "//". We check separately for ending '/' with the
                 * `is_dir` variable. */
                "" => (),
                /* Skip over redundant "." separators. */
                "." => (),
                /* If ".." is present, pop off the last element or return an error. */
                ".." => {
                    if ret.pop().is_none() {
                        return Err(PathSplitError::PathFormat(format!(
                        "path {:?} has too many '..' components and would escape the containing dir",
                        entry_path
                    )));
                    }
                }
                _ => {
                    ret.push(component);
                }
            }
        }
        if ret.is_empty() {
            return Err(PathSplitError::PathFormat(format!(
                "path {:?} resolves to the top-level directory",
                entry_path
            )));
        }

        Ok((ret, is_dir))
    }

    fn split_dir_file_components<'a, 's>(
        all_components: &'s [&'a str],
        is_dir: bool,
    ) -> (&'s [&'a str], Option<&'a str>) {
        if is_dir {
            (all_components, None)
        } else {
            let (last, rest) = all_components.split_last().unwrap();
            (rest, Some(last))
        }
    }

    #[derive(PartialEq, Eq, Debug)]
    pub(crate) struct DirEntry<'a, Data> {
        pub properties: Option<Data>,
        pub children: BTreeMap<&'a str, Box<FSEntry<'a, Data>>>,
    }

    impl<'a, Data> Default for DirEntry<'a, Data> {
        fn default() -> Self {
            Self {
                properties: None,
                children: BTreeMap::new(),
            }
        }
    }

    #[derive(PartialEq, Eq, Debug)]
    pub(crate) enum FSEntry<'a, Data> {
        Dir(DirEntry<'a, Data>),
        File(Data),
    }

    /* This returns a BTreeMap and not a DirEntry because we do not allow setting permissions or
     * any other data for the top-level extraction directory. */
    pub(crate) fn lexicographic_entry_trie<'a, Data>(
        all_paths: impl IntoIterator<Item = (&'a str, Data)>,
    ) -> Result<BTreeMap<&'a str, Box<FSEntry<'a, Data>>>, PathSplitError> {
        let mut base_dir: DirEntry<'a, Data> = DirEntry::default();

        for (entry_path, data) in all_paths {
            let mut cur_dir = &mut base_dir;

            let (all_components, is_dir) = normalize_parent_dirs(entry_path)?;
            let (dir_components, file_component) =
                split_dir_file_components(&all_components, is_dir);
            for component in dir_components.iter() {
                let next_subdir = cur_dir
                    .children
                    .entry(component)
                    .or_insert_with(|| Box::new(FSEntry::Dir(DirEntry::default())));
                cur_dir = match next_subdir.as_mut() {
                    &mut FSEntry::File(_) => {
                        return Err(PathSplitError::FileDirOverlap(format!(
                            "a file was already registered at the same path as the dir entry {:?}",
                            entry_path
                        )));
                    }
                    &mut FSEntry::Dir(ref mut subdir) => subdir,
                }
            }
            match file_component {
                Some(filename) => {
                    /* We can't handle duplicate file paths, as that might mess up our
                     * parallelization strategy. */
                    if let Some(prev_entry) = cur_dir.children.get(filename) {
                        return Err(PathSplitError::FileDirOverlap(format!(
                            "another file or directory was already registered at the same path as the file entry {:?}",
                            entry_path
                        )));
                    }
                    cur_dir
                        .children
                        .insert(filename, Box::new(FSEntry::File(data)));
                }
                None => {
                    /* We can't handle duplicate directory entries for the exact same normalized
                     * path, as it's not clear how to merge the possibility of two separate file
                     * permissions. */
                    if let Some(_) = cur_dir.properties.replace(data) {
                        return Err(PathSplitError::FileDirOverlap(format!(
                            "another directory was already registered at the path {:?}",
                            entry_path
                        )));
                    }
                }
            }
        }

        let DirEntry {
            properties,
            children,
        } = base_dir;
        assert!(properties.is_none(), "setting metadata on the top-level extraction dir is not allowed and should have been filtered out");
        Ok(children)
    }

    /* TODO: use proptest for all of this! */
    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn path_normalization() {
            assert_eq!(
                normalize_parent_dirs("a/b/c").unwrap(),
                (vec!["a", "b", "c"], false)
            );
            assert_eq!(normalize_parent_dirs("./a").unwrap(), (vec!["a"], false));
            assert_eq!(normalize_parent_dirs("a/../b/").unwrap(), (vec!["b"], true));
            assert_eq!(normalize_parent_dirs("a\\").unwrap(), (vec!["a"], true));
            assert!(normalize_parent_dirs("/a").is_err());
            assert!(normalize_parent_dirs("\\a").is_err());
            assert!(normalize_parent_dirs("a\\b/").is_err());
            assert!(normalize_parent_dirs("a/../../b").is_err());
            assert!(normalize_parent_dirs("./").is_err());
        }

        #[test]
        fn split_dir_file() {
            assert_eq!(
                split_dir_file_components(&["a", "b", "c"], true),
                (["a", "b", "c"].as_ref(), None)
            );
            assert_eq!(
                split_dir_file_components(&["a", "b", "c"], false),
                (["a", "b"].as_ref(), Some("c"))
            );
        }

        #[test]
        fn lex_trie() {
            assert_eq!(
                lexicographic_entry_trie([
                    ("a/b/", 1),
                    ("a/", 2),
                    ("a/b/c", 3),
                    ("d/", 4),
                    ("e", 5),
                    ("a/b/f/g", 6),
                ])
                .unwrap(),
                [
                    (
                        "a",
                        FSEntry::Dir(DirEntry {
                            properties: Some(2),
                            children: [(
                                "b",
                                FSEntry::Dir(DirEntry {
                                    properties: Some(1),
                                    children: [
                                        ("c", FSEntry::File(3).into()),
                                        (
                                            "f",
                                            FSEntry::Dir(DirEntry {
                                                properties: None,
                                                children: [("g", FSEntry::File(6).into())]
                                                    .into_iter()
                                                    .collect(),
                                            })
                                            .into()
                                        ),
                                    ]
                                    .into_iter()
                                    .collect(),
                                })
                                .into()
                            )]
                            .into_iter()
                            .collect(),
                        })
                        .into()
                    ),
                    (
                        "d",
                        FSEntry::Dir(DirEntry {
                            properties: Some(4),
                            children: BTreeMap::new(),
                        })
                        .into()
                    ),
                    ("e", FSEntry::File(5).into())
                ]
                .into_iter()
                .collect()
            );
        }
    }
}

mod handle_creation {
    use displaydoc::Display;
    use thiserror::Error;

    use std::cmp;
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
    use std::fs;
    use std::hash;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::path_splitting::{DirEntry, FSEntry};

    use crate::types::{ffi::S_IFLNK, ZipFileData};

    /// Errors encountered when creating output handles for extracting entries to.
    #[derive(Debug, Display, Error)]
    pub enum HandleCreationError {
        /// i/o error: {0}
        Io(#[from] io::Error),
        /// extraction dir {0:?} existed but was not writable
        ExtractionDirNotWritable(PathBuf),
    }

    pub(crate) struct ZipDataHandle<'a>(&'a ZipFileData);

    impl<'a> ZipDataHandle<'a> {
        #[inline(always)]
        const fn ptr(&self) -> *const ZipFileData {
            self.0
        }

        #[inline(always)]
        pub const fn wrap(data: &'a ZipFileData) -> Self {
            Self(data)
        }
    }

    impl<'a> cmp::PartialEq for ZipDataHandle<'a> {
        #[inline(always)]
        fn eq(&self, other: &Self) -> bool {
            self.ptr() == other.ptr()
        }
    }

    impl<'a> cmp::Eq for ZipDataHandle<'a> {}

    impl<'a> hash::Hash for ZipDataHandle<'a> {
        #[inline(always)]
        fn hash<H: hash::Hasher>(&self, state: &mut H) {
            self.ptr().hash(state);
        }
    }

    pub(crate) struct AllocatedHandles<'a> {
        pub file_handle_mapping: HashMap<ZipDataHandle<'a>, fs::File>,
        pub symlink_entries: HashSet<ZipDataHandle<'a>>,
    }

    pub(crate) fn transform_entries_to_allocated_handles<'a>(
        lex_entry_trie: BTreeMap<&'a str, Box<FSEntry<'a, &'a ZipFileData>>>,
        top_level_extraction_dir: &Path,
    ) -> Result<AllocatedHandles<'a>, HandleCreationError> {
        #[cfg(unix)]
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        /* NB: we create subdirs by constructing path strings, which may fail at overlarge
         * paths. This may be fixable on unix with mkdirat()/openat(), but would require more
         * complex platform-specific programming. However, the result would likely decrease the
         * number of syscalls, which may also improve performance. It may also be slightly easier to
         * follow the logic if we can refer to directory inodes instead of constructing path strings
         * as a proxy. This should be considered if requested by users. */
        fs::create_dir_all(top_level_extraction_dir)?;

        /* Directories must be writable until all normal files are extracted. We will reset the
         * perms to their original value after extraction. */
        /* TODO: reuse logic from ZipArchive::make_writable_dir_all()! */
        let original_top_level_perms = fs::metadata(top_level_extraction_dir)?.permissions();
        if original_top_level_perms.readonly() {
            return Err(HandleCreationError::ExtractionDirNotWritable(
                top_level_extraction_dir.to_path_buf(),
            ));
        }
        #[cfg(unix)]
        fs::set_permissions(
            top_level_extraction_dir,
            fs::Permissions::from_mode(0o700 | original_top_level_perms.mode()),
        )?;
        #[cfg(windows)]
        {
            let mut writable_perms = original_top_level_perms.clone();
            writable_perms.set_readonly(false);
            fs::set_permissions(top_level_extraction_dir, writable_perms)?;
        }

        let mut file_handle_mapping: HashMap<ZipDataHandle<'a>, fs::File> = HashMap::new();
        let mut symlink_entries: HashSet<ZipDataHandle<'a>> = HashSet::new();
        /* TODO: parallelize this using a channel! */
        /* NB: the parent dir perms are necessary to propagate because we may temporarily set
         * writable perms in order to extract, but want to avoid mutating perms for any subdirs
         * without explicit entries after extraction. */
        let mut entry_queue: VecDeque<(
            PathBuf,
            fs::Permissions,
            Box<FSEntry<'a, &'a ZipFileData>>,
        )> = lex_entry_trie
            .into_iter()
            .map(|(entry_name, entry_data)| {
                (
                    top_level_extraction_dir.join(entry_name),
                    original_top_level_perms.clone(),
                    entry_data,
                )
            })
            .collect();
        /* Reset extraction dir perms to their original value before this method was called, and
         * set non-writable and other perms as specified by directory entries. */
        let mut dir_perms_todo: Vec<(PathBuf, fs::Permissions)> = vec![(
            top_level_extraction_dir.to_path_buf(),
            original_top_level_perms,
        )];

        while let Some((path, parent_dir_perms, entry)) = entry_queue.pop_front() {
            match *entry {
                FSEntry::File(data) => {
                    let key = ZipDataHandle::wrap(data);
                    let mut opts = fs::OpenOptions::new();

                    if let Some(mode) = data.unix_mode() {
                        /* TODO: reuse logic from ZipFile::is_symlink()! */
                        if mode & S_IFLNK == S_IFLNK {
                            /* FIXME: This is actually harder than it sounds, because an archive may
                             * have an entry with a path that indexes into the symlink path and not
                             * the symlink's target path. We can probably just disallow this for
                             * pipelined extraction, but we probably *do* want to support symlinks
                             * in general. We may want to perform a preprocessing step somehow. */
                            /* FIXME: add a preprocessing step to error out if any entry *after*
                             * a symlink entry for a directory refers to the symlink path; this
                             * will work with normal extraction bc it goes in order, but not with
                             * parallel extraction. */
                            assert!(symlink_entries.insert(key));
                            continue;
                        }

                        /* TODO: consider handling the readonly bit on windows. We don't currently
                         * do this in normal extraction, so we don't need to do this yet for
                         * pipelining. */
                        #[cfg(unix)]
                        opts.mode(mode);
                    }
                    opts.write(true).create(true).truncate(true);

                    let handle = opts.open(path)?;
                    assert!(file_handle_mapping.insert(key, handle).is_none());
                }
                FSEntry::Dir(DirEntry {
                    properties,
                    children,
                }) => {
                    let perms_to_set = match properties.and_then(|data| data.unix_mode()) {
                        Some(mode) => {
                            #[cfg(unix)]
                            let ret_perms = fs::Permissions::from_mode(mode);
                            /* On windows, just propagate the parent dir perms. */
                            #[cfg(windows)]
                            let ret_perms = parent_dir_perms;
                            ret_perms
                        }
                        None => match fs::metadata(&path) {
                            Err(e) if e.kind() == io::ErrorKind::NotFound => parent_dir_perms,
                            Err(e) => return Err(e.into()),
                            Ok(metadata) => metadata.permissions(),
                        },
                    };
                    match fs::create_dir(&path) {
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                        Err(e) => return Err(e.into()),
                        Ok(()) => (),
                    }
                    #[cfg(unix)]
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(0o700 | perms_to_set.mode()),
                    )?;
                    #[cfg(windows)]
                    {
                        let mut writable_perms = perms_to_set.clone();
                        writable_perms.set_readonly(false);
                        fs::set_permissions(&path, writable_perms)?;
                    }

                    /* (1) Write the desired perms to the dir perms queue. */
                    dir_perms_todo.push((path.clone(), perms_to_set.clone()));
                    /* (2) Generate sub-entries by constructing full paths. */
                    for (sub_name, entry) in children.into_iter() {
                        let full_name = path.join(sub_name);
                        entry_queue.push_back((full_name, perms_to_set.clone(), entry));
                    }
                }
            }
        }

        for (dir_path, perms) in dir_perms_todo.into_iter().rev() {
            fs::set_permissions(dir_path, perms)?;
        }

        Ok(AllocatedHandles {
            file_handle_mapping,
            symlink_entries,
        })
    }
}
