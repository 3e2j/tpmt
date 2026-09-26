//! The files tpmt writes into a project and reads back later, one type per
//! file:
//!
//! ```text
//! base/disc.toml      DiscMetadata   preamble values a build cannot derive
//! base/yaz0.toml      Yaz0           which loose files arrived Yaz0 wrapped
//! mod/mod.json        ModMetadata    id, name, version, author, ...
//! .tpmt/source.toml   Source         where the ISO was last seen, plus its sha1
//! .tpmt/hashes.toml   Hashes         vanilla sha1 of every base/ file
//! .tpmt/formats.toml  Formats        which base/ files hold a known leaf format
//! ```
//!
//! `disc.toml` and `mod.json` are safe to edit by hand. `.tpmt/` is not:
//! every unpack rewrites it.
//!
//! Unpack writes all of these and build reads them back, so each type here
//! goes both ways.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{DISC_TOML, FORMATS_TOML, HASHES_TOML, MOD_JSON, SOURCE_TOML, STORE_DIR, YAZ0_TOML};
use crate::fs::{io_at, read_toml, write_json, write_toml};
use crate::progress::{Progress, Step};
use crate::{Error, FileKind, Result};

/// `yaz0.toml`: which loose files arrived Yaz0 wrapped. Recorded here
/// because a loose file never records its own wrapper (unlike containers).
///
/// A set rather than a list, since the only question anyone asks it is
/// whether one path is in it.
#[derive(Serialize, Deserialize)]
struct Yaz0 {
    compressed: BTreeSet<String>,
}

/// Writes `base/`'s own metadata: `disc.toml` and `yaz0.toml`. The one call
/// site for everything under `base/` that isn't a copied file, so nothing
/// else reaches into `base/` to write a TOML of its own.
pub fn write_base(
    base: &Path,
    metadata: &tpmt_disc::Metadata,
    yaz0_compressed: BTreeSet<String>,
) -> Result<()> {
    write_toml(&base.join(DISC_TOML), metadata)?;
    write_toml(
        &base.join(YAZ0_TOML),
        &Yaz0 {
            compressed: yaz0_compressed,
        },
    )
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

/// Reads back what [`write_base`] wrote.
///
/// # Errors
///
/// - [`Error::Io`](crate::Error::Io) if either file is missing
/// - [`Error::Parse`](crate::Error::Parse) if either is not what it was
pub fn read_base(base: &Path) -> Result<Base> {
    let metadata = read_toml(&base.join(DISC_TOML))?;
    let yaz0: Yaz0 = read_toml(&base.join(YAZ0_TOML))?;
    Ok(Base {
        metadata,
        yaz0_compressed: yaz0.compressed,
    })
}

/// The disc's boot header from `disc.toml`, without reading `yaz0.toml`.
///
/// # Errors
///
/// - [`Error::Io`](crate::Error::Io) if `disc.toml` is missing
/// - [`Error::Parse`](crate::Error::Parse) if it is not what it was
pub fn read_boot(base: &Path) -> Result<tpmt_disc::Boot> {
    let metadata: tpmt_disc::Metadata = read_toml(&base.join(DISC_TOML))?;
    Ok(metadata.boot)
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
#[derive(Serialize, Deserialize)]
pub struct Source {
    pub iso: PathBuf,
    pub sha1: String,
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
    formats: &Formats,
) -> Result<()> {
    let iso = iso.canonicalize().map_err(io_at(iso))?;
    let store = project.join(STORE_DIR);
    write_toml(&store.join(HASHES_TOML), hashes)?;
    let named: BTreeMap<_, _> = formats
        .iter()
        .map(|(kind, paths)| (kind.name(), paths))
        .collect();
    write_toml(&store.join(FORMATS_TOML), &named)?;
    write_toml(
        &store.join(SOURCE_TOML),
        &Source {
            iso,
            sha1: sha1.to_string(),
        },
    )
}

/// What `.tpmt/` holds: the disc this project came from, and what every file
/// the unpack wrote hashed to.
pub struct Store {
    pub source: Source,
    /// The vanilla sha1 of every project file, keyed by project path. Around
    /// 27,000 entries for one disc.
    pub hashes: BTreeMap<String, String>,
}

/// Reads back what [`write_store`] wrote.
///
/// # Errors
///
/// - [`Error::Io`](crate::Error::Io) if either file is missing
/// - [`Error::Parse`](crate::Error::Parse) if either is not what it was
pub fn read_store(project: &Path) -> Result<Store> {
    let store = project.join(STORE_DIR);
    Ok(Store {
        source: read_toml(&store.join(SOURCE_TOML))?,
        hashes: read_toml(&store.join(HASHES_TOML))?,
    })
}

/// `formats.toml`: every `base/` file whose magic a [`FileKind`] recognised,
/// grouped by kind. A file no kind recognises isn't listed. On disk each kind
/// is its [`FileKind::name`], since `tpmt-format` carries no serde.
///
/// It exists because a name on the disc can't be trusted: some files carry
/// no extension, or one that doesn't match what's inside. Only the magic
/// can. Unpack already holds every file's bytes, so it reads each magic once
/// and records it here, and a lookup by kind never reopens `base/`.
pub type Formats = BTreeMap<FileKind, BTreeSet<String>>;

/// Reads back the `formats.toml` [`write_store`] wrote. Apart from
/// [`read_store`], since a lookup by kind has no use for 27,000 hashes.
///
/// # Errors
///
/// - [`Error::Io`](crate::Error::Io) if it is missing
/// - [`Error::Parse`](crate::Error::Parse) if it is not what it was, or names a
///   kind this build doesn't know
pub fn read_formats(project: &Path) -> Result<Formats> {
    let path = project.join(STORE_DIR).join(FORMATS_TOML);
    let named: BTreeMap<String, BTreeSet<String>> = read_toml(&path)?;
    named
        .into_iter()
        .map(|(name, paths)| {
            let kind = FileKind::from_name(&name).ok_or_else(|| Error::Parse {
                path: path.clone(),
                source: format!("no file kind is named `{name}`").into(),
            })?;
            Ok((kind, paths))
        })
        .collect()
}

/// The digest `hashes.toml` records per project file.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// [`sha1_hex`] of a file, streamed rather than read whole. Status hashes
/// every file in the project, videos included.
pub fn sha1_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).map_err(io_at(path))?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut std::io::BufReader::new(file), &mut hasher).map_err(io_at(path))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// The digest `source.toml` records for the disc, reported as
/// [`Step::HashDisc`].
pub fn sha1_disc(disc: &tpmt_disc::Disc, progress: &Progress) -> Result<String> {
    let hashing = progress.begin(Step::HashDisc, disc.len());
    Ok(disc.sha1(|size| hashing.add(size))?)
}
