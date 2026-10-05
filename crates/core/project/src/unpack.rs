//! What an unpack writes into a project: a fresh `vanilla/`, then `.tpmt/`,
//! then `mod/` if it is missing.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use tpmt_binary::Compression;

use crate::io::{Staging, fs};
use crate::layout::{modding, store, vanilla};
use crate::path::checked;
use crate::{FileKind, Project, Result};

/// A `vanilla/` being written beside the old one, swapped in by
/// [`finish`](Self::finish). Dropping it unfinished leaves the old `vanilla/`
/// as it was.
///
/// Takes writes from several threads at once.
pub struct NewVanilla {
    staging: Staging,
}

/// What an unpack wrote for one file, for [`Project::write_store`] to record.
#[derive(Debug)]
pub struct Written {
    /// Its project path.
    pub path: String,
    pub digest: u128,
    /// What its magic says it is, if anything.
    pub kind: Option<FileKind>,
}

impl NewVanilla {
    /// Writes one file at its project path.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`](crate::Error::UnusablePath) if `path` is
    ///   empty, absolute, or climbs out
    /// - [`Error::Io`](crate::Error::Io) on the write
    pub fn write(&self, path: &str, kind: Option<FileKind>, bytes: &[u8]) -> Result<Written> {
        fs::write(&self.staging.dir().join(checked(path)?), bytes)?;
        Ok(Written {
            path: path.to_string(),
            digest: store::digest(bytes),
            kind,
        })
    }

    /// Makes a directory at its project path. Writing a file already makes
    /// its parents, so this is only needed for an empty one.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub fn create_dir(&self, path: &str) -> Result<()> {
        fs::create_dir_all(&self.staging.dir().join(checked(path)?))
    }

    /// Writes `disc.toml` and `compression.toml`, then swaps the new `vanilla/`
    /// in for the old one.
    ///
    /// `disc` is whatever the disc's preamble holds that a build can't derive.
    /// This crate stores it without knowing its shape.
    ///
    /// # Errors
    ///
    /// - [`Error::Serialize`](crate::Error::Serialize) if either will not
    ///   serialize
    /// - [`Error::Io`](crate::Error::Io) on any write or the swap
    pub fn finish(
        self,
        disc: &impl Serialize,
        compressed: &BTreeMap<String, Compression>,
    ) -> Result<()> {
        vanilla::write(self.staging.dir(), disc, compressed)?;
        self.staging.promote()
    }
}

impl Project {
    /// Takes `root` for an unpack to write a project into, refusing it if it
    /// is not a project but already holds files.
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
    /// - [`Error::Io`](crate::Error::Io) if it cannot be listed
    pub fn claim(root: &Path) -> Result<Self> {
        crate::discover::refuse_foreign(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Starts a fresh `vanilla/` beside the current one.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`](crate::Error::Io) if the staging directory cannot be
    ///   made
    pub fn new_vanilla(&self) -> Result<NewVanilla> {
        Ok(NewVanilla {
            staging: Staging::begin(&self.vanilla())?,
        })
    }

    /// Writes `.tpmt/`, which is what makes this a project, from what the
    /// unpack wrote. Run it last, once every other file is in place.
    ///
    /// Stores the ISO path canonicalized so later commands can read files off
    /// the original disc without asking the user where it is again. `id` and
    /// `revision` are the game id and revision the disc held.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`](crate::Error::Io) if `iso` cannot be canonicalized, or
    ///   on any write
    /// - [`Error::UnusablePath`](crate::Error::UnusablePath) if a path would
    ///   not read back from a line of its own
    /// - [`Error::Serialize`](crate::Error::Serialize) if a file will not
    ///   serialize
    pub fn write_store(
        &self,
        iso: &Path,
        id: &str,
        revision: u8,
        written: Vec<Written>,
    ) -> Result<()> {
        let mut digests = store::Digests::new();
        let mut payloads = store::Payloads::new();
        for Written { path, digest, kind } in written {
            if let Some(payload) = kind.and_then(FileKind::payload) {
                payloads.entry(payload).or_default().insert(path.clone());
            }
            digests.insert(path, digest);
        }
        store::write(&self.store(), iso, id, revision, &digests, &payloads)
    }

    /// Writes the `mod/` skeleton (`changes/`, `textures/`, `res/scripts/`, a
    /// starter `mod.json`) alongside `vanilla/`. Skips an existing `mod/`, so
    /// re-unpacking a project never clobbers a modder's edits.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`](crate::Error::Io) if any of it cannot be written
    /// - [`Error::Serialize`](crate::Error::Serialize) if `mod.json` will not
    ///   serialize
    pub fn scaffold_mod(&self) -> Result<()> {
        let id = self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mod");
        modding::scaffold(&self.root.join(modding::DIR), id)
    }
}
