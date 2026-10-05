//! Replacing a directory whole: the new copy is written beside it and only
//! swapped in once complete.

use std::fs;
use std::path::{Path, PathBuf};

use super::fs::{create_dir_all, remove_dir_all_if_exists, rename_if_exists};
use crate::{Error, Result};

/// Appended to a directory's name for the copy [`Staging`] writes.
const STAGING_SUFFIX: &str = ".tpmt-tmp";
/// Appended to a directory's name for the copy a promote moves aside.
const REPLACED_SUFFIX: &str = ".tpmt-old";

/// A fresh copy of a directory being written beside it under a temporary
/// name, swapped in by [`promote`](Self::promote) once complete and thrown
/// away otherwise.
///
/// Dropping one unpromoted removes whatever it wrote, so a failed unpack or
/// build leaves the directory it was replacing as it found it.
///
/// The temporary names carry a `tpmt` suffix because a build output can be
/// anywhere, and `begin` clears whatever sits at the staging name.
pub struct Staging {
    target: PathBuf,
    dir: PathBuf,
}

impl Staging {
    /// Clears anything a previous failed attempt left behind and opens a
    /// fresh staging directory beside `target`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `target` has no name to stage beside
    /// - [`Error::Io`] if the staging directory cannot be made
    pub fn begin(target: &Path) -> Result<Self> {
        let dir = beside(target, STAGING_SUFFIX)?;
        remove_dir_all_if_exists(&dir)?;
        create_dir_all(&dir)?;
        Ok(Self {
            target: target.to_path_buf(),
            dir,
        })
    }

    /// Where the new contents go.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What [`promote`](Self::promote) replaces.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Swaps the staged tree in as the target. Moves the old one aside
    /// rather than deleting it first, so a failure between the two renames
    /// leaves it recoverable under a `.tpmt-old` name.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if the target has no name to set aside
    /// - [`Error::Io`] if either rename fails, or a leftover will not go
    pub fn promote(self) -> Result<()> {
        let old = beside(&self.target, REPLACED_SUFFIX)?;

        remove_dir_all_if_exists(&old)?;
        rename_if_exists(&self.target, &old)?;
        fs::rename(&self.dir, &self.target).map_err(Error::io(&self.target))?;
        remove_dir_all_if_exists(&old)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Best effort. After a promote there is nothing here, and after a
        // failure the error that caused it matters more.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `target` with `suffix` on the end of its name.
fn beside(target: &Path, suffix: &str) -> Result<PathBuf> {
    let mut name = target
        .file_name()
        .ok_or_else(|| Error::UnusablePath(target.to_path_buf()))?
        .to_os_string();
    name.push(suffix);
    Ok(target.with_file_name(name))
}

/// Refuses `dir` if it holds any name outside `owned`, counting a
/// [`Staging`] copy of an owned name as owned. A missing or empty directory
/// passes.
///
/// # Errors
///
/// - [`Error::ForeignDirectory`] if `dir` holds a name outside `owned`
/// - [`Error::Io`] if it cannot be listed
pub fn refuse_unowned(dir: &Path, owned: &[&str]) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(Error::io(dir))? {
        let entry = entry.map_err(Error::io(dir))?;
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| owned.contains(&unstaged(name)))
        {
            return Err(Error::ForeignDirectory(dir.to_path_buf()));
        }
    }
    Ok(())
}

/// `name` without a [`Staging`] suffix, if it has one.
fn unstaged(name: &str) -> &str {
    [STAGING_SUFFIX, REPLACED_SUFFIX]
        .into_iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::fs::{read, write};

    #[test]
    fn dropped_staging_leaves_no_trace() {
        let scratch = tempfile::tempdir().unwrap();
        let staging = Staging::begin(&scratch.path().join("vanilla")).unwrap();
        assert_eq!(staging.dir(), scratch.path().join("vanilla.tpmt-tmp"));
        fs::write(staging.dir().join("half"), b"written").unwrap();
        drop(staging);

        assert!(!scratch.path().join("vanilla.tpmt-tmp").exists());
        assert!(!scratch.path().join("vanilla").exists());
    }

    #[test]
    fn promoted_staging_replaces_the_target() {
        let scratch = tempfile::tempdir().unwrap();
        let vanilla = scratch.path().join("vanilla");
        write(&vanilla.join("stale"), b"old").unwrap();

        let staging = Staging::begin(&vanilla).unwrap();
        write(&staging.dir().join("fresh"), b"new").unwrap();
        staging.promote().unwrap();

        assert_eq!(read(&vanilla.join("fresh")).unwrap(), b"new");
        assert!(!vanilla.join("stale").exists());
        assert!(!scratch.path().join("vanilla.tpmt-tmp").exists());
        assert!(!scratch.path().join("vanilla.tpmt-old").exists());
    }
}
