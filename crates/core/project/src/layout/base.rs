//! `base/`: the read-only unpack of the disc, and the two files that record
//! what its unpacked files can't:
//!
//! ```text
//! disc.toml          preamble values a build cannot derive, as packing hands them over
//! compression.toml   which loose files arrived wrapped, in what (path = "yaz0")
//! ```
//!
//! `disc.toml` is safe to edit by hand. Every unpack rewrites `base/` whole.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tpmt_binary::Compression;

use crate::Result;
use crate::io::fs::{read_toml, write_toml};

pub const DIR: &str = "base";
const DISC_TOML: &str = "disc.toml";
/// Recorded here because a loose file never records its own wrapper (unlike
/// an archive member, whose sidecar does).
const COMPRESSION_TOML: &str = "compression.toml";

/// What `base/` says about itself, which is everything a rebuild needs that
/// the unpacked files do not carry.
pub struct Base<D> {
    /// The preamble values a build cannot derive.
    pub disc: D,
    /// Which disc files arrived compressed, so a rebuild puts the same
    /// wrapper back on each.
    pub compressed: BTreeMap<String, Compression>,
}

/// Writes `disc.toml` and `compression.toml` into `dir`, a `base/` being
/// staged.
///
/// The one call site for everything under `base/` that isn't a copied file,
/// so nothing else reaches into `base/` to write a TOML of its own.
///
/// # Errors
///
/// - [`Error::Serialize`](crate::Error::Serialize) if either will not serialize
/// - [`Error::Io`](crate::Error::Io) on either write
pub fn write(
    dir: &Path,
    disc: &impl Serialize,
    compressed: &BTreeMap<String, Compression>,
) -> Result<()> {
    write_toml(&dir.join(DISC_TOML), disc)?;
    write_toml(&dir.join(COMPRESSION_TOML), compressed)
}

pub fn read<D: DeserializeOwned>(dir: &Path) -> Result<Base<D>> {
    Ok(Base {
        disc: read_disc(dir)?,
        compressed: read_toml(&dir.join(COMPRESSION_TOML))?,
    })
}

/// `disc.toml` alone, without reading `compression.toml`.
pub fn read_disc<D: DeserializeOwned>(dir: &Path) -> Result<D> {
    read_toml(&dir.join(DISC_TOML))
}
