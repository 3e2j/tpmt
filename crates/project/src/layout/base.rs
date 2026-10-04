//! `base/`: the read-only unpack of the disc, and the two files that record
//! what its unpacked files can't:
//!
//! ```text
//! disc.toml          tpmt_disc::Metadata   preamble values a build cannot derive
//! compression.toml   path = "yaz0"         which loose files arrived wrapped, in what
//! ```
//!
//! `disc.toml` is safe to edit by hand. Every unpack rewrites `base/` whole.

use std::collections::BTreeMap;
use std::path::Path;

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
pub struct Base {
    /// The preamble values a build cannot derive.
    pub metadata: tpmt_disc::Metadata,
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
    metadata: &tpmt_disc::Metadata,
    compressed: &BTreeMap<String, Compression>,
) -> Result<()> {
    write_toml(&dir.join(DISC_TOML), metadata)?;
    write_toml(&dir.join(COMPRESSION_TOML), compressed)
}

pub fn read(dir: &Path) -> Result<Base> {
    let metadata = read_toml(&dir.join(DISC_TOML))?;
    let compressed = read_toml(&dir.join(COMPRESSION_TOML))?;
    Ok(Base {
        metadata,
        compressed,
    })
}

/// The disc's boot header from `disc.toml`, without reading `compression.toml`.
pub fn read_boot(dir: &Path) -> Result<tpmt_disc::Boot> {
    let metadata: tpmt_disc::Metadata = read_toml(&dir.join(DISC_TOML))?;
    Ok(metadata.boot)
}
