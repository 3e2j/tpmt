//! Filesystem mechanics shared across the pipeline: reading a file whole,
//! writing one into place, and staging output so nothing half written is
//! ever left where somebody could mistake it for finished work.
//!
//! Nothing here knows what a project, a store, or a sidecar is; it only
//! knows paths and bytes, which is what lets all three build on it as peers
//! rather than one becoming a special case of another.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::Error;

/// Somewhere to generate into, on the same filesystem as where it is going.
///
/// Nothing half written is left where a person could take it for finished
/// output, and nothing half written blocks the next attempt: the work happens
/// under a name nobody is looking at, and the last step is a rename. A rename
/// is only atomic within one filesystem, and on Windows fails outright across
/// volumes, so this stays beside its target rather than in a temp directory.
pub(crate) struct Staged {
    path: PathBuf,
    target: PathBuf,
    directory: bool,
}

impl Staged {
    /// A directory to fill in, cleared of whatever the last attempt left.
    ///
    /// One already sitting there is only replaced if everything in it is named
    /// in `ours`, which is what says it is output rather than a directory
    /// somebody keeps things in. Output paths can be named on the command line,
    /// so the target is not always somewhere a build has been before, and
    /// finishing means deleting whatever is in the way. An empty list allows
    /// only an empty directory.
    pub(crate) fn directory(target: &Path, ours: &[&str]) -> Result<Self, Error> {
        if target.is_dir() {
            let theirs = listing(target)?
                .into_iter()
                .any(|name| !ours.contains(&name.as_str()));
            if theirs {
                return Err(Error::ForeignDirectory(target.to_path_buf()));
            }
        }

        let staged = Self::beside(target, true)?;
        create_dir(&staged.path)?;
        Ok(staged)
    }

    /// A file to write, which the caller creates itself.
    pub(crate) fn file(target: &Path) -> Result<Self, Error> {
        let staged = Self::beside(target, false)?;
        if let Some(parent) = staged.path.parent() {
            create_dir(parent)?;
        }
        Ok(staged)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Puts the finished work where it was always going.
    pub(crate) fn finish(self) -> Result<PathBuf, Error> {
        // A rename replaces a file, but not a directory, so one that is already
        // there goes first. Only ever our own output, under a name a build
        // chose.
        if self.directory {
            self.clear(&self.target)?;
        }
        fs::rename(&self.path, &self.target).map_err(|source| Error::Write {
            path: self.target.clone(),
            source,
        })?;
        Ok(self.target)
    }

    fn beside(target: &Path, directory: bool) -> Result<Self, Error> {
        let mut name = target.file_name().unwrap_or_default().to_os_string();
        name.push(".part");

        let staged = Self {
            path: target.with_file_name(name),
            target: target.to_path_buf(),
            directory,
        };
        staged.clear(&staged.path)?;
        Ok(staged)
    }

    fn clear(&self, path: &Path) -> Result<(), Error> {
        let gone = match (path.exists(), self.directory) {
            (false, _) => return Ok(()),
            (true, true) => fs::remove_dir_all(path),
            (true, false) => fs::remove_file(path),
        };
        gone.map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// Reads a file whole.
///
/// Everything read this way is one of the project's own files, which are game
/// assets rather than disc images: the largest is a few tens of megabytes.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let read = |source| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    File::open(path)
        .map_err(read)?
        .read_to_end(&mut bytes)
        .map_err(read)?;
    Ok(bytes)
}

/// What a directory holds, by name.
fn listing(path: &Path) -> Result<Vec<String>, Error> {
    let failed = |source| Error::Read {
        path: path.to_path_buf(),
        source,
    };

    let mut names = Vec::new();
    for entry in fs::read_dir(path).map_err(failed)? {
        names.push(
            entry
                .map_err(failed)?
                .file_name()
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(names)
}

pub(crate) fn create_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    fs::write(path, bytes).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}
