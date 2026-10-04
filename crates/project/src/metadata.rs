//! The files tpmt writes into a project and reads back later, one type per
//! file:
//!
//! ```text
//! base/disc.toml      DiscMetadata   preamble values a build cannot derive
//! base/yaz0.toml      Yaz0           which loose files arrived Yaz0 wrapped
//! mod/mod.json        ModMetadata    id, name, version, author, ...
//! .tpmt/source.toml   Source         where the ISO was last seen, and which game
//! .tpmt/digests      Digests        vanilla digest of every base/ file
//! .tpmt/formats      Formats        which base/ files hold a known leaf format
//! ```
//!
//! `disc.toml` and `mod.json` are safe to edit by hand. `.tpmt/` is not:
//! every unpack rewrites it.
//!
//! Unpack writes all of these and build reads them back, so each type here
//! goes both ways.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::fs::{self, io_at, parse_at, read_toml, write_json, write_toml};
use crate::layout::{DIGESTS, DISC_TOML, FORMATS, MOD_JSON, SOURCE_TOML, STORE_DIR, YAZ0_TOML};
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
///
/// # Errors
///
/// - [`Error::Serialize`] if either will not serialize
/// - [`Error::Io`] on either write
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
/// - [`Error::Io`] if either file is missing
/// - [`Error::Parse`] if either is not what it was
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
/// - [`Error::Io`] if `disc.toml` is missing
/// - [`Error::Parse`] if it is not what it was
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

/// Writes `mod.json` into `mod_dir`.
///
/// # Errors
///
/// - [`Error::Serialize`] if it will not serialize
/// - [`Error::Io`] on the write
pub fn write_mod(mod_dir: &Path, metadata: &ModMetadata<'_>) -> Result<()> {
    write_json(&mod_dir.join(MOD_JSON), metadata)
}

/// `source.toml`: where the ISO this project came from was last seen, and
/// the game id and revision it held, so a build can tell if it moved or now
/// holds another version.
///
/// Kept apart from `disc.toml`, which a modder may edit to rename the build.
#[derive(Serialize, Deserialize)]
pub struct Source {
    pub iso: PathBuf,
    pub id: String,
    pub revision: u8,
}

impl Source {
    /// Whether `boot` is the version this project was unpacked from.
    #[must_use]
    pub fn matches(&self, boot: &tpmt_disc::Boot) -> bool {
        self.id == boot.id && self.revision == boot.revision
    }
}

/// Writes `.tpmt/`, which is what makes `project` a project. The caller
/// runs this last, once every other file is in place.
///
/// Stores the ISO path canonicalized so later commands can read files off
/// the original disc without asking the user where it is again.
///
/// # Errors
///
/// - [`Error::Io`] if `iso` cannot be canonicalized, or on any write
/// - [`Error::UnusablePath`] if a path would not read back from a line of its own
/// - [`Error::Serialize`] if a file will not serialize
pub fn write_store(
    project: &Path,
    iso: &Path,
    boot: &tpmt_disc::Boot,
    digests: &Digests,
    formats: &Formats,
) -> Result<()> {
    let iso = iso.canonicalize().map_err(io_at(iso))?;
    let store = project.join(STORE_DIR);
    write_digests(&store.join(DIGESTS), digests)?;
    write_formats(&store.join(FORMATS), formats)?;
    write_toml(
        &store.join(SOURCE_TOML),
        &Source {
            iso,
            id: boot.id.clone(),
            revision: boot.revision,
        },
    )
}

/// What `.tpmt/` holds: the disc this project came from, and what every file
/// the unpack wrote hashed to.
pub struct Store {
    pub source: Source,
    pub digests: Digests,
}

/// Reads back what [`write_store`] wrote.
///
/// # Errors
///
/// - [`Error::Io`] if either file is missing
/// - [`Error::Parse`] if either is not what it was
pub fn read_store(project: &Path) -> Result<Store> {
    let store = project.join(STORE_DIR);
    Ok(Store {
        source: read_toml(&store.join(SOURCE_TOML))?,
        digests: read_digests(&store.join(DIGESTS))?,
    })
}

/// `digests`: the vanilla [`digest`] of every project file, keyed by project
/// path. Around 27,000 entries for one disc.
///
/// On disk it is one `<32 hex digits>  <path>` line per file, sorted by path.
/// Nobody edits it by hand, so it skips TOML
pub type Digests = BTreeMap<String, u128>;

/// A digest as the 32 hex digits it is written as, with no `String` built per
/// line.
struct Hex(u128);

impl Display for Hex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

fn write_digests(path: &Path, digests: &Digests) -> Result<()> {
    let mut text = String::new();
    for (file, digest) in digests {
        writeln!(text, "{}  {}", Hex(*digest), one_line(file)?).map_err(|source| {
            Error::Serialize {
                path: path.to_path_buf(),
                source: source.into(),
            }
        })?;
    }
    fs::write(path, text.as_bytes())
}

fn read_digests(path: &Path) -> Result<Digests> {
    read_text(path)?
        .lines()
        .map(|line| {
            let (digest, file) = line
                .split_once("  ")
                .ok_or_else(|| parse_at(path)(format!("`{line}` is not a digest and a path")))?;
            let digest = u128::from_str_radix(digest, 16).map_err(parse_at(path))?;
            Ok((file.to_string(), digest))
        })
        .collect()
}

/// `formats`: every `base/` file whose magic a [`FileKind`] recognised,
/// grouped by kind. A file no kind recognises isn't listed.
///
/// On disk each kind is a `[name]` line with its paths one per line under
/// it, sorted by kind and then path.
///
/// It exists because a name on the disc can't be trusted: some files carry
/// no extension, or one that doesn't match what's inside. Only the magic
/// can. Unpack already holds every file's bytes, so it reads each magic once
/// and records it here, and a lookup by kind never reopens `base/`.
pub type Formats = BTreeMap<FileKind, BTreeSet<String>>;

fn write_formats(path: &Path, formats: &Formats) -> Result<()> {
    let mut text = String::new();
    for (kind, files) in formats {
        text.push('[');
        text.push_str(kind.name());
        text.push_str("]\n");
        for file in files {
            text.push_str(one_line(file)?);
            text.push('\n');
        }
    }
    fs::write(path, text.as_bytes())
}

/// Reads back the `formats` [`write_store`] wrote. Apart from
/// [`read_store`], since a lookup by kind has no use for 27,000 digests.
///
/// # Errors
///
/// - [`Error::Io`] if it is missing
/// - [`Error::Parse`] if it is not what it was, or names a
///   kind this build doesn't know
pub fn read_formats(project: &Path) -> Result<Formats> {
    let path = project.join(STORE_DIR).join(FORMATS);
    let mut formats = Formats::new();
    let mut files = None;
    for line in read_text(&path)?.lines() {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let kind = FileKind::from_name(name)
                .ok_or_else(|| parse_at(&path)(format!("no file kind is named `{name}`")))?;
            files = Some(formats.entry(kind).or_default());
        } else {
            files
                .as_mut()
                .ok_or_else(|| parse_at(&path)(format!("`{line}` comes before any `[kind]`")))?
                .insert(line.to_string());
        }
    }
    Ok(formats)
}

/// `file`, if it reads back as itself from a line of its own. A newline would
/// split it in two, and a leading `[` would read as a `formats` group.
fn one_line(file: &str) -> Result<&str> {
    if file.contains('\n') || file.starts_with('[') {
        return Err(Error::UnusablePath(file.into()));
    }
    Ok(file)
}

fn read_text(path: &Path) -> Result<String> {
    String::from_utf8(fs::read(path)?).map_err(parse_at(path))
}

/// The digest [`Digests`] records per project file.
///
/// XXH3-128 rather than a cryptographic hash, since it only has to catch
/// edits, not forgeries.
#[must_use]
pub fn digest(data: &[u8]) -> u128 {
    xxh3_128(data)
}

/// [`digest`] of a file, streamed rather than read whole. Status hashes
/// every file in the project, videos included.
///
/// # Errors
///
/// - [`Error::Io`] if the file cannot be opened or read
pub fn digest_file(path: &Path) -> Result<u128> {
    let file = std::fs::File::open(path).map_err(io_at(path))?;
    let mut hasher = Xxh3::new();
    std::io::copy(&mut std::io::BufReader::new(file), &mut hasher).map_err(io_at(path))?;
    Ok(hasher.digest128())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formats_path(project: &Path) -> PathBuf {
        project.join(STORE_DIR).join(FORMATS)
    }

    #[test]
    fn formats_read_back_as_written() {
        let scratch = tempfile::tempdir().unwrap();
        let formats = Formats::from([
            (
                FileKind::Mesg,
                BTreeSet::from(["files/a.arc/b.bmg".to_string(), "files/c.bmg".to_string()]),
            ),
            (FileKind::Rarc, BTreeSet::from(["files/d.arc".to_string()])),
        ]);
        write_formats(&formats_path(scratch.path()), &formats).unwrap();

        assert_eq!(read_formats(scratch.path()).unwrap(), formats);
    }
}
