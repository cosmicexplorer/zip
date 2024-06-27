//! Pipelined extraction into a filesystem directory.

use displaydoc::Display;
use thiserror::Error;

use std::collections::BTreeMap;
use std::io;

use crate::read::ZipArchive;
use crate::spec::is_dir;

/// Errors encountered during pipelined extraction.
#[derive(Debug, Display, Error)]
pub enum PipelinedExtractionError {
    /// i/o error: {0}
    Io(#[from] io::Error),
    /// entry path format error: {0}
    PathFormat(String),
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

enum FSEntry<'a> {
    Dir {
        name: &'a str,
        contents: BTreeMap<&'a str, Box<FSEntry<'a>>>,
    },
    File(&'a str),
}

fn lexicographic_entry_trie<'a, R>(
    archive: &'a ZipArchive<R>,
) -> Result<BTreeMap<&'a str, Box<FSEntry<'a>>>, PipelinedExtractionError> {
    let mut base_dir: BTreeMap<&'a str, Box<FSEntry<'a>>> = BTreeMap::new();

    for entry_path in archive.shared.files.keys() {
        let cur_dir = &mut base_dir;

        let (components, is_dir) = normalize_parent_dirs(entry_path)?;
        todo!();
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
}
