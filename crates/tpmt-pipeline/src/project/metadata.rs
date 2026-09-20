//! The files tpmt writes into a project and reads back later, one type per
//! file:
//!
//! ```text
//! base/disc.toml      DiscMetadata   preamble values a build cannot derive
//! base/yaz0.toml      Yaz0           which loose files arrived Yaz0 wrapped
//! mod/mod.json        ModMetadata    id, name, version, author, ...
//! .tpmt/source.toml   Source         where the ISO was last seen, plus its sha1
//! .tpmt/hashes.toml   Hashes         vanilla sha1 of every base/ file
//! ```
//!
//! `disc.toml` and `mod.json` are safe to edit by hand. `.tpmt/` is not:
//! every unpack rewrites it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use sha1::{Digest, Sha1};

use super::{DISC_TOML, HASHES_TOML, MOD_JSON, SOURCE_TOML, STORE_DIR, YAZ0_TOML};
use crate::Result;
use crate::fs::{io_at, write_json, write_toml};

/// `yaz0.toml`: which loose files arrived Yaz0 wrapped. Recorded here
/// because a loose file never records its own wrapper (unlike containers).
#[derive(Serialize)]
struct Yaz0<'a> {
    compressed: &'a [String],
}

/// Writes `base/`'s own metadata: `disc.toml` and `yaz0.toml`. The one call
/// site for everything under `base/` that isn't a copied file, so nothing
/// else reaches into `base/` to write a TOML of its own.
pub fn write_base(
    base: &Path,
    metadata: &tpmt_disc::Metadata,
    yaz0_compressed: &[String],
) -> Result<()> {
    write_toml(&base.join(DISC_TOML), metadata)?;
    write_toml(
        &base.join(YAZ0_TOML),
        &Yaz0 {
            compressed: yaz0_compressed,
        },
    )
}

/// `mod.json`: what a mod says about itself.
///
/// Fields are the target-agnostic subset only. Dusklight reads a few more
/// (`runtime`, pinning a mod to a specific host runtime service) that are
/// specific to the `.dusk` export step.
#[derive(Serialize)]
pub struct ModMetadata<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub author: &'a str,
    pub description: &'a str,
    pub icon: Option<&'a str>,
    pub banner: Option<&'a str>,
}

pub fn write_mod(mod_dir: &Path, metadata: &ModMetadata<'_>) -> Result<()> {
    write_json(&mod_dir.join(MOD_JSON), metadata)
}

/// `source.toml`: where the ISO this project came from was last seen, and
/// its sha1, so a build can tell if it moved or changed.
#[derive(Serialize)]
struct Source<'a> {
    iso: &'a Path,
    sha1: &'a str,
}

/// Writes `.tpmt/`, which is what makes `project` a project. The caller
/// runs this last, once every other file is in place.
///
/// Stores the ISO path canonicalized so later commands can read files off
/// the original disc without asking the user where it is again.
pub fn write_store(
    project: &Path,
    iso: &Path,
    sha1: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<()> {
    let iso = iso.canonicalize().map_err(io_at(iso))?;
    let store = project.join(STORE_DIR);
    write_toml(&store.join(HASHES_TOML), hashes)?;
    write_toml(&store.join(SOURCE_TOML), &Source { iso: &iso, sha1 })
}

/// The digest `hashes.toml` records per project file.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
