//! The project directory: what a person edits, and how it is read back.
//!
//! Everything here is about the folder itself, not about what a build does with
//! it: the file at the top of it, the two directories under it, and the rule
//! that nothing half written is ever left where somebody could mistake it for
//! finished output.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tpmt_disc::{Bi2, Boot, Metadata};

use crate::Error;

/// Bumped whenever a field in the project file moves or changes meaning, so an
/// old project is told so rather than quietly misread.
const SCHEMA: u32 = 1;

pub(crate) const PROJECT_FILE: &str = "tpmt.toml";
/// The game's own files, and the two preamble pieces, mirroring the disc.
pub(crate) const FILES: &str = "files";
pub(crate) const SYS: &str = "sys";
/// Where finished mods and images land, which is somewhere a person looks.
pub(crate) const OUT: &str = "out";

/// The project file: what a build needs that is not a file in the project.
///
/// The disc's boot header and metadata are nine kilobytes of mostly nothing, so
/// they are kept as their values instead, which also puts the title and the
/// region somewhere a person can edit them.
#[derive(Serialize, Deserialize)]
pub(crate) struct Project {
    schema: u32,
    boot: Boot,
    bi2: Bi2,
}

impl Project {
    pub(crate) fn new(metadata: &Metadata) -> Self {
        Self {
            schema: SCHEMA,
            boot: metadata.boot.clone(),
            bi2: metadata.bi2.clone(),
        }
    }

    /// Reads the project file, refusing one written by a version that meant
    /// something else by these fields.
    ///
    /// A missing file is what says this directory is not a project at all,
    /// since it is the one thing every project has.
    pub(crate) fn read(project: &Path) -> Result<Metadata, Error> {
        let path = project.join(PROJECT_FILE);
        if !path.exists() {
            return Err(Error::NotAProject(project.to_path_buf()));
        }

        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        let project: Self = toml::from_str(&text).map_err(|source| Error::UnreadableProject {
            path: path.clone(),
            source,
        })?;

        match project.schema == SCHEMA {
            true => Ok(Metadata {
                boot: project.boot,
                bi2: project.bi2,
            }),
            false => Err(Error::Schema {
                found: project.schema,
                want: SCHEMA,
            }),
        }
    }

    pub(crate) fn write(&self, project: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self)?;
        write(&project.join(PROJECT_FILE), text.as_bytes())
    }
}

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
                return Err(Error::NotOurs(target.to_path_buf()));
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
