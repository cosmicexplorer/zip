//! Pipelined extraction into a filesystem directory.

use displaydoc::Display;
use thiserror::Error;

use std::collections::BTreeMap;
use std::io;

use crate::spec::is_dir;

/// Errors encountered during pipelined extraction.
#[derive(Debug, Display, Error)]
pub enum PipelinedExtractionError {
    /// i/o error: {0}
    Io(#[from] io::Error),
    /// entry path format error: {0}
    PathFormat(String),
    /// file and directory paths overlapped: {0}
    FileDirOverlap(String),
}

fn split_by_separator<'a>(
    entry_path: &'a str,
) -> Result<impl Iterator<Item = &'a str>, PipelinedExtractionError> {
    if entry_path.contains('\\') {
        if entry_path.contains('/') {
            return Err(PipelinedExtractionError::PathFormat(format!(
                "path {:?} contained both '\\' and '/' separators",
                entry_path
            )));
        }
        Ok(entry_path.split('\\'))
    } else {
        Ok(entry_path.split('/'))
    }
}

fn normalize_parent_dirs<'a>(
    entry_path: &'a str,
) -> Result<(Vec<&'a str>, bool), PipelinedExtractionError> {
    if entry_path.starts_with('/') || entry_path.starts_with('\\') {
        return Err(PipelinedExtractionError::PathFormat(format!(
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
                    return Err(PipelinedExtractionError::PathFormat(format!(
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
        return Err(PipelinedExtractionError::PathFormat(format!(
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

enum FSEntry<'a> {
    Dir(BTreeMap<&'a str, Box<FSEntry<'a>>>),
    File,
}

fn lexicographic_entry_trie<'a>(
    all_paths: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<&'a str, Box<FSEntry<'a>>>, PipelinedExtractionError> {
    let mut base_dir: BTreeMap<&'a str, Box<FSEntry<'a>>> = BTreeMap::new();

    for entry_path in all_paths {
        let mut cur_dir = &mut base_dir;

        let (all_components, is_dir) = normalize_parent_dirs(entry_path)?;
        let (dir_components, file_component) = split_dir_file_components(&all_components, is_dir);
        for component in dir_components.iter() {
            let next_subdir = cur_dir
                .entry(component)
                .or_insert_with(|| Box::new(FSEntry::Dir(BTreeMap::new())));
            cur_dir = match next_subdir.as_mut() {
                &mut FSEntry::File => {
                    return Err(PipelinedExtractionError::FileDirOverlap(format!(
                        "a file was already registered at the same path as the dir entry {:?}",
                        entry_path
                    )));
                }
                &mut FSEntry::Dir(ref mut contents) => contents,
            }
        }
        if let Some(filename) = file_component {
            /* We can't handle duplicate file paths, as that might mess up our parallelization
             * strategy. */
            if let Some(prev_entry) = cur_dir.get(filename) {
                return Err(PipelinedExtractionError::FileDirOverlap(format!(
                    "another file or directory was already registered at the same path as the file entry {:?}",
                    entry_path
                )));
            }
            cur_dir.insert(filename, Box::new(FSEntry::File));
        }
    }

    Ok(base_dir)
}

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
    fn lex_trie() {}
}
