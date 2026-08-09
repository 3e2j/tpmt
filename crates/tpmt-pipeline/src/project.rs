//! The project directory: what a person edits, and how it is read back.
//!
//! The file at the top of it, and the two directories under it. How any of
//! this gets written to disk safely is [`crate::fs`]'s job, not this one.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tpmt_disc::{Bi2, Boot, Metadata};

use crate::Error;
use crate::fs::{read, write};

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
            false => Err(Error::SchemaMismatch {
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
