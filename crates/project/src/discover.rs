//! Telling a project from any other directory: finding one above a path,
//! and refusing to unpack over a directory that holds someone else's files.

use std::path::{Path, PathBuf};

use crate::io::fs::io_at;
use crate::io::refuse_unowned;
use crate::{BUILD_DIR, Error, Result, base, mod_dir, store};

/// Every top-level name this crate writes. A directory holding nothing but
/// these and their [`Staging`](crate::io::Staging) copies is ours, however
/// far an unpack got before it failed.
const OWNED: [&str; 4] = [base::DIR, mod_dir::DIR, BUILD_DIR, store::DIR];

/// Whether `dir` is a finished unpack: it has the `.tpmt/` that only
/// [`Project::write_store`](crate::Project::write_store) writes, and only
/// after everything else is in place.
#[must_use]
pub fn is_project(dir: &Path) -> bool {
    dir.join(store::DIR).is_dir()
}

/// Finds the project root by walking upward from `start`. Canonicalizes
/// first, then climbs one directory at a time until a `.tpmt` store turns
/// up or the climb hits the filesystem root.
pub fn discover(start: &Path) -> Result<PathBuf> {
    let mut at = start.canonicalize().map_err(io_at(start))?;

    loop {
        if is_project(&at) {
            return Ok(at);
        }
        if !at.pop() {
            return Err(Error::NoProjectFound(start.to_path_buf()));
        }
    }
}

/// Refuses a directory that is not a project but already holds files. See
/// [`Project::claim`](crate::Project::claim).
pub fn refuse_foreign(project: &Path) -> Result<()> {
    if is_project(project) {
        return Ok(());
    }
    refuse_unowned(project, &OWNED)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::io::fs::write;

    fn mark_project(dir: &Path) {
        fs::create_dir_all(dir.join(store::DIR)).unwrap();
    }

    #[test]
    fn finds_the_root_from_itself() {
        let scratch = tempfile::tempdir().unwrap();
        mark_project(scratch.path());

        let found = discover(scratch.path()).unwrap();
        assert_eq!(found, scratch.path().canonicalize().unwrap());
    }

    #[test]
    fn finds_the_root_from_a_subdirectory() {
        let scratch = tempfile::tempdir().unwrap();
        mark_project(scratch.path());
        let nested = scratch
            .path()
            .join(base::DIR)
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

    /// Unpacking into a directory the user already keeps their own files in
    /// fails rather than clearing it to make room.
    #[test]
    fn refuses_a_directory_that_is_not_a_project() {
        let scratch = tempfile::tempdir().unwrap();
        let target = scratch.path().join("mine");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("notes.txt"), b"do not delete").unwrap();

        let error = refuse_foreign(&target).unwrap_err();
        assert!(matches!(error, Error::ForeignDirectory(path) if path == target));
        assert!(target.join("notes.txt").exists());
    }

    /// An empty directory has nothing in it to protect, and a project is
    /// what an unpack is for.
    #[test]
    fn accepts_empty_missing_and_project_directories() {
        let scratch = tempfile::tempdir().unwrap();
        let empty = scratch.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let project = scratch.path().join("project");
        mark_project(&project);
        fs::write(project.join("anything"), b"").unwrap();

        refuse_foreign(&empty).unwrap();
        refuse_foreign(&scratch.path().join("missing")).unwrap();
        refuse_foreign(&project).unwrap();
    }

    /// An unpack that died before the store went in leaves only names this
    /// crate wrote, so the next attempt can carry on rather than refuse.
    #[test]
    fn accepts_a_half_finished_unpack() {
        let scratch = tempfile::tempdir().unwrap();
        write(&scratch.path().join(base::DIR).join("files").join("a"), b"").unwrap();
        write(
            &scratch.path().join("base.tpmt-tmp").join("files").join("a"),
            b"",
        )
        .unwrap();
        fs::create_dir_all(scratch.path().join(mod_dir::DIR)).unwrap();

        refuse_foreign(scratch.path()).unwrap();
    }
}
