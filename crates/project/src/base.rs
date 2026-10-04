//! `base/`: the read-only unpack of the disc, and the two files that record
//! what its unpacked files can't:
//!
//! ```text
//! disc.toml   tpmt_disc::Metadata   preamble values a build cannot derive
//! yaz0.toml   Yaz0                  which loose files arrived Yaz0 wrapped
//! ```
//!
//! `disc.toml` is safe to edit by hand. Every unpack rewrites `base/` whole.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::io::fs::{read_toml, write_toml};

pub const DIR: &str = "base";
const DISC_TOML: &str = "disc.toml";
const YAZ0_TOML: &str = "yaz0.toml";

/// `yaz0.toml`. Recorded here because a loose file never records its own
/// wrapper (unlike containers).
///
/// A set rather than a list, since the only question anyone asks it is
/// whether one path is in it.
#[derive(Serialize, Deserialize)]
struct Yaz0 {
    compressed: BTreeSet<String>,
}

/// What `base/` says about itself, which is everything a rebuild needs that
/// the unpacked files do not carry.
pub struct Base {
    /// The preamble values a build cannot derive.
    pub metadata: tpmt_disc::Metadata,
    /// Which disc files arrived Yaz0 wrapped, so a rebuild puts the wrapper
    /// back on the same ones.
    pub yaz0_compressed: BTreeSet<String>,
}

/// Writes `disc.toml` and `yaz0.toml` into `dir`, a `base/` being staged.
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
    yaz0_compressed: BTreeSet<String>,
) -> Result<()> {
    write_toml(&dir.join(DISC_TOML), metadata)?;
    write_toml(
        &dir.join(YAZ0_TOML),
        &Yaz0 {
            compressed: yaz0_compressed,
        },
    )
}

pub fn read(dir: &Path) -> Result<Base> {
    let metadata = read_toml(&dir.join(DISC_TOML))?;
    let yaz0: Yaz0 = read_toml(&dir.join(YAZ0_TOML))?;
    Ok(Base {
        metadata,
        yaz0_compressed: yaz0.compressed,
    })
}

/// The disc's boot header from `disc.toml`, without reading `yaz0.toml`.
pub fn read_boot(dir: &Path) -> Result<tpmt_disc::Boot> {
    let metadata: tpmt_disc::Metadata = read_toml(&dir.join(DISC_TOML))?;
    Ok(metadata.boot)
}
