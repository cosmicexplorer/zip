//! Pipelined extraction into a filesystem directory.

mod path_splitting {
    use displaydoc::Display;
    use thiserror::Error;

    use std::collections::BTreeMap;

    use crate::spec::is_dir;

    /// Errors encountered during path splitting
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
        properties: Option<Data>,
        children: BTreeMap<&'a str, Box<FSEntry<'a, Data>>>,
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
