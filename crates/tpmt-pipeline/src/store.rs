//! The store: what the toolkit generates about a project, rather than for a
//! person to edit.
//!
//! Two things, both taken at unpack and both read by every build: where the
//! disc came from, and what every file in the project looked like before
//! anybody touched it.
//!
//! It sits inside the project so that a rename into place stays on one
//! filesystem, and under a dotted name so it stays out of the way of the files
//! that are meant to be edited.
//!
//! Flat, rather than a directory per disc. One project is one disc: the id and
//! the revision are values a person can edit in tpmt.toml, so a path built out
//! of them stops leading anywhere the moment somebody does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::project::{read, write};

/// Everything generated about a project, out of the way of what is edited.
const STORE: &str = ".tpmt";
const SOURCE_FILE: &str = "source.toml";
const HASHES_FILE: &str = "hashes";

/// Where a project's disc was last seen, and which disc it was.
///
/// Neither ending can copy a file verbatim without this: an unchanged file is
/// bytes off the original disc, so the original disc has to be findable and has
/// to still be the one the project was unpacked from.
#[derive(Serialize, Deserialize)]
pub(crate) struct Source {
    pub(crate) path: PathBuf,
    /// Over the image, so a container and a raw dump of the same disc agree.
    pub(crate) sha1: String,
}

/// One project's store.
pub(crate) struct Store {
    root: PathBuf,
}

impl Store {
    pub(crate) fn new(project: &Path) -> Self {
        Self {
            root: project.join(STORE),
        }
    }

    pub(crate) fn write_source(&self, source: &Source) -> Result<(), Error> {
        let text = toml::to_string_pretty(source)?;
        write(&self.root.join(SOURCE_FILE), text.as_bytes())
    }

    pub(crate) fn source(&self) -> Result<Source, Error> {
        let path = self.root.join(SOURCE_FILE);
        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        toml::from_str(&text).map_err(|source| Error::UnreadableProject { path, source })
    }

    /// The vanilla hashes, keyed by project path, over the bytes as they were
    /// written into the project. Comparing a file against its own is the whole
    /// of change detection.
    ///
    /// One line each, since there is one for every file in the project and for
    /// every file inside every archive.
    pub(crate) fn write_hashes(&self, hashes: &[(String, String)]) -> Result<(), Error> {
        let mut hashes = hashes.to_vec();
        hashes.sort();

        let mut text = String::with_capacity(hashes.len() * 64);
        for (path, hash) in hashes {
            text.push_str(&hash);
            text.push(' ');
            text.push_str(&path);
            text.push('\n');
        }
        write(&self.root.join(HASHES_FILE), text.as_bytes())
    }

    pub(crate) fn hashes(&self) -> Result<HashMap<String, String>, Error> {
        let path = self.root.join(HASHES_FILE);
        let text = String::from_utf8_lossy(&read(&path)?).into_owned();

        let mut hashes = HashMap::new();
        for line in text.lines() {
            let (hash, file) = line
                .split_once(' ')
                .ok_or_else(|| Error::CorruptStore(path.clone()))?;
            hashes.insert(file.to_string(), hash.to_string());
        }
        Ok(hashes)
    }
}
