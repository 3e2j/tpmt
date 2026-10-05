//! Telling a project from any other directory, and finding one above a path.

use std::path::{Path, PathBuf};

use crate::layout::store;
use crate::{Error, Result};

/// Whether `dir` is a finished unpack: it has the `.tpmt/` that an unpack
/// writes last, once everything else is in place.
#[must_use]
pub fn is_project(dir: &Path) -> bool {
    dir.join(store::DIR).is_dir()
}

/// Finds the project root by walking upward from `start`. Canonicalizes
/// first, then climbs one directory at a time until a `.tpmt` store turns
/// up or the climb hits the filesystem root.
pub fn discover(start: &Path) -> Result<PathBuf> {
    let mut at = start.canonicalize().map_err(Error::io(start))?;

    loop {
        if is_project(&at) {
            return Ok(at);
        }
        if !at.pop() {
            return Err(Error::NoProjectFound(start.to_path_buf()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::layout::vanilla;

    fn mark_project(dir: &Path) {
        fs::create_dir_all(dir.join(store::DIR)).unwrap();
    }

    #[test]
    fn finds_the_root_from_a_subdirectory() {
        let scratch = tempfile::tempdir().unwrap();
        mark_project(scratch.path());
        let nested = scratch
            .path()
            .join(vanilla::DIR)
            .join("files")
            .join("thing.arc");
        fs::create_dir_all(&nested).unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, scratch.path().canonicalize().unwrap());
    }

    #[test]
    fn refuses_a_directory_with_no_project_above_it() {
        let scratch = tempfile::tempdir().unwrap();

        let error = discover(scratch.path()).unwrap_err();
        assert!(matches!(error, Error::NoProjectFound(_)));
    }
}
