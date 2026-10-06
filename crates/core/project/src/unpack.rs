//! What an unpack writes into a project: a fresh `vanilla/`, then `.tpmt/`,
//! then `mod/` if it is missing.
//!
//! [`Unpacking`] owns that order, so nothing outside this crate can write
//! `.tpmt/` before the files it vouches for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tpmt_binary::Compression;

use crate::discover::is_project;
use crate::io::{Staging, fs, refuse_unowned};
use crate::layout::{self, modding, store, vanilla};
use crate::path::checked;
use crate::{FileKind, Project, Result};

/// A project being written by an unpack. Its `vanilla/` is staged beside
/// the old one, and [`finish`](Self::finish) swaps it in. Dropping it
/// unfinished leaves the old project as it was.
///
/// Takes writes from several threads at once.
pub struct Unpacking {
    root: PathBuf,
    vanilla: Staging,
}

/// What an unpack wrote for one file, for [`Unpacking::finish`] to record.
#[derive(Debug)]
pub struct Written {
    path: String,
    digest: u128,
    /// What its magic says it is, if anything.
    kind: Option<FileKind>,
}

/// What the disc says about itself, which [`Unpacking::finish`] records.
pub struct Record<'a, D> {
    /// The game image unpacked. Stored canonicalized so a later build can read
    /// files off it without asking where it is again.
    pub game_image: &'a Path,
    /// The game id and revision it held.
    pub id: &'a str,
    pub revision: u8,
    /// The preamble values a build can't derive, stored as
    /// `vanilla/disc.toml` without this crate knowing their shape.
    pub disc_metadata: &'a D,
    /// The disc files that arrived compressed, and with which wrapper.
    pub compressed: &'a BTreeMap<String, Compression>,
    /// Every directory the disc lists, so an empty one survives.
    pub directories: &'a [String],
}

impl Project {
    /// Starts an unpack into `root`, refusing it if it is not a project but
    /// already holds files.
    ///
    /// A project passes whatever else it holds (notes, fixtures, `.git`), since
    /// a re-unpack replaces only `vanilla/`. An empty or missing directory passes,
    /// as does one holding only names this crate writes, from an unpack that
    /// failed part way.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`](crate::Error::ForeignDirectory) if it
    ///   holds anything else
    /// - [`Error::Io`](crate::Error::Io) if it cannot be listed, or the
    ///   staging directory cannot be made
    pub fn unpack(root: &Path) -> Result<Unpacking> {
        refuse_foreign(root)?;
        Ok(Unpacking {
            root: root.to_path_buf(),
            vanilla: Staging::begin(&root.join(vanilla::DIR))?,
        })
    }
}

impl Unpacking {
    /// Writes one file into `vanilla/` at its project path.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`](crate::Error::UnusablePath) if `path` is
    ///   empty, absolute, or climbs out
    /// - [`Error::Io`](crate::Error::Io) on the write
    pub fn write(&self, path: &str, kind: Option<FileKind>, bytes: &[u8]) -> Result<Written> {
        fs::write(&self.vanilla.dir().join(checked(path)?), bytes)?;
        Ok(Written {
            path: path.to_string(),
            digest: store::digest(bytes),
            kind,
        })
    }

    /// Records `record` and `written`, swaps the new `vanilla/` in, writes
    /// `.tpmt/`, which is what makes this a project, and then the `mod/`
    /// skeleton if there is none. Returns the finished project.
    ///
    /// An existing `mod/` is never touched, so re-unpacking a project never
    /// clobbers a modder's edits.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`](crate::Error::UnusablePath) if a directory
    ///   path is unusable, or a file path would not read back from a line of
    ///   its own
    /// - [`Error::Serialize`](crate::Error::Serialize) if a generated file
    ///   will not serialize
    /// - [`Error::Io`](crate::Error::Io) if `game_image` cannot be canonicalized, or
    ///   on any write or the swap
    pub fn finish<D: Serialize>(
        self,
        record: &Record<'_, D>,
        written: Vec<Written>,
    ) -> Result<Project> {
        let Self { root, vanilla } = self;
        for dir in record.directories {
            fs::create_dir_all(&vanilla.dir().join(checked(dir)?))?;
        }
        vanilla::write(vanilla.dir(), record.disc_metadata, record.compressed)?;
        vanilla.promote()?;

        let mut digests = store::Digests::new();
        let mut payloads = store::Payloads::new();
        for Written { path, digest, kind } in written {
            if let Some(payload) = kind.and_then(FileKind::payload) {
                payloads.entry(payload).or_default().insert(path.clone());
            }
            digests.insert(path, digest);
        }
        store::write(
            &root.join(store::DIR),
            record.game_image,
            record.id,
            record.revision,
            &digests,
            &payloads,
        )?;

        let id = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mod");
        modding::scaffold(&root.join(modding::DIR), id)?;

        Project::discover(&root)
    }
}

/// Refuses a directory that is not a project but already holds files. See
/// [`Project::unpack`].
fn refuse_foreign(project: &Path) -> Result<()> {
    if is_project(project) {
        return Ok(());
    }
    refuse_unowned(project, &layout::OWNED)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::Error;
    use crate::io::fs::write;

    fn mark_project(dir: &Path) {
        fs::create_dir_all(dir.join(store::DIR)).unwrap();
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

    /// A re-unpack replaces only `vanilla/`, so whatever else a project holds
    /// is safe. A missing directory is what `tpmt new` usually gets.
    #[test]
    fn accepts_a_project_with_other_files() {
        let scratch = tempfile::tempdir().unwrap();
        let project = scratch.path().join("project");
        mark_project(&project);
        fs::write(project.join("anything"), b"").unwrap();

        refuse_foreign(&project).unwrap();
        refuse_foreign(&scratch.path().join("missing")).unwrap();
    }

    /// An unpack that died before the store went in leaves only names this
    /// crate wrote, so the next attempt can carry on rather than refuse.
    #[test]
    fn accepts_a_half_finished_unpack() {
        let scratch = tempfile::tempdir().unwrap();
        write(
            &scratch.path().join(vanilla::DIR).join("files").join("a"),
            b"",
        )
        .unwrap();
        write(
            &scratch
                .path()
                .join("vanilla.tpmt-tmp")
                .join("files")
                .join("a"),
            b"",
        )
        .unwrap();
        fs::create_dir_all(scratch.path().join(modding::DIR)).unwrap();

        refuse_foreign(scratch.path()).unwrap();
    }
}
