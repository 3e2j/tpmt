//! What an unpack writes into a project: a fresh `vanilla/`, then `.tpmt/`,
//! then `mod/` if it is missing.
//!
//! [`Unpacking`] owns that order, so nothing outside this crate can write
//! `.tpmt/` before the files it vouches for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tpmt_binary::Compression;

use crate::io::{Staging, fs};
use crate::layout::{modding, store, vanilla};
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
    /// The disc unpacked. Stored canonicalized so a later build can read
    /// files off it without asking where it is again.
    pub disc: &'a Path,
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
        crate::discover::refuse_foreign(root)?;
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
    /// - [`Error::Io`](crate::Error::Io) if `iso` cannot be canonicalized, or
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
            record.disc,
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
