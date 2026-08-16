//! The manifest: what a person edits at the top of a project, and how it is
//! read back. Also the two directories under it, since they are part of the
//! same layout.
//!
//! How any of this gets written to disk safely is [`crate::fs`]'s job, not
//! this one.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tpmt_disc::{Bi2, Boot, Metadata};

use crate::Error;
use crate::fs::{read, write};

pub(crate) const PROJECT_FILE: &str = "tpmt.toml";
/// The game's own files, and the two preamble pieces, mirroring the disc.
pub(crate) const FILES: &str = "files";
pub(crate) const SYS: &str = "sys";
/// Where finished mods and images land, which is somewhere a person looks.
pub(crate) const OUT: &str = "out";

/// The manifest: what a build needs that is not a file in the project.
///
/// The disc's boot header and metadata are nine kilobytes of mostly nothing, so
/// they are kept as their values instead, which also puts the title and the
/// region somewhere a person can edit them.
#[derive(Serialize, Deserialize)]
pub(crate) struct Manifest {
    boot: Boot,
    bi2: Bi2,
}

impl Manifest {
    pub(crate) fn new(metadata: &Metadata) -> Self {
        Self {
            boot: metadata.boot.clone(),
            bi2: metadata.bi2.clone(),
        }
    }

    /// Reads the manifest.
    ///
    /// A missing file is what says this directory is not a project at all,
    /// since it is the one thing every project has.
    // TODO: no version fallback here yet. Once the manifest's shape
    // changes, an older project needs a way to still be read rather than
    // just failing to parse.
    pub(crate) fn read(project: &Path) -> Result<Metadata, Error> {
        let path = project.join(PROJECT_FILE);
        if !path.exists() {
            return Err(Error::NotAProject(project.to_path_buf()));
        }

        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        let manifest: Self = toml::from_str(&text).map_err(|source| Error::UnreadableProject {
            path: path.clone(),
            source,
        })?;

        Ok(Metadata {
            boot: manifest.boot,
            bi2: manifest.bi2,
        })
    }

    pub(crate) fn write(&self, project: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self)?;
        write(&project.join(PROJECT_FILE), text.as_bytes())
    }
}
