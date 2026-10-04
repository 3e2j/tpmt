//! `.tpmt/`: everything an unpack records about the project rather than for
//! it, one type per file:
//!
//! ```text
//! source.toml      Source    where the ISO was last seen, and which game
//! digests.xxh128   Digests   vanilla digest of every base/ file
//! formats          Formats   which base/ files hold a known leaf format
//! ```
//!
//! None of it is safe to edit by hand: every unpack rewrites it. Its presence
//! is what marks an unpack as finished, so it is always written last.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::io::fs::{self, read_toml, write_toml};
use crate::{Error, FileKind, Result};

pub const DIR: &str = ".tpmt";
const FORMATS: &str = "formats";
const DIGESTS: &str = "digests.xxh128";
const SOURCE_TOML: &str = "source.toml";

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

/// What `.tpmt/` holds: the disc this project came from, and what every file
/// the unpack wrote hashed to.
pub struct Store {
    pub source: Source,
    pub digests: Digests,
}

pub fn write(
    dir: &Path,
    iso: &Path,
    boot: &tpmt_disc::Boot,
    digests: &Digests,
    formats: &Formats,
) -> Result<()> {
    let iso = iso.canonicalize().map_err(Error::io(iso))?;
    write_digests(&dir.join(DIGESTS), digests)?;
    write_formats(&dir.join(FORMATS), formats)?;
    write_toml(
        &dir.join(SOURCE_TOML),
        &Source {
            iso,
            id: boot.id.clone(),
            revision: boot.revision,
        },
    )
}

pub fn read(dir: &Path) -> Result<Store> {
    Ok(Store {
        source: read_toml(&dir.join(SOURCE_TOML))?,
        digests: read_digests(&dir.join(DIGESTS))?,
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
            let (digest, file) = line.split_once("  ").ok_or_else(|| {
                Error::parse(path)(format!("`{line}` is not a digest and a path"))
            })?;
            let digest = u128::from_str_radix(digest, 16).map_err(Error::parse(path))?;
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

/// Reads `formats` alone, since a lookup by kind has no use for 27,000
/// digests.
pub fn read_formats(dir: &Path) -> Result<Formats> {
    let path = dir.join(FORMATS);
    let mut formats = Formats::new();
    let mut files = None;
    for line in read_text(&path)?.lines() {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let kind = FileKind::from_name(name)
                .ok_or_else(|| Error::parse(&path)(format!("no file kind is named `{name}`")))?;
            files = Some(formats.entry(kind).or_default());
        } else {
            files
                .as_mut()
                .ok_or_else(|| Error::parse(&path)(format!("`{line}` comes before any `[kind]`")))?
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
    String::from_utf8(fs::read(path)?).map_err(Error::parse(path))
}

/// The digest [`Digests`] records per project file.
///
/// XXH3-128 rather than a cryptographic hash, since it only has to catch
/// edits, not forgeries.
#[must_use]
pub fn digest(data: &[u8]) -> u128 {
    xxh3_128(data)
}

/// [`digest`] of a file, streamed rather than read whole. A diff hashes
/// every overlay file, videos included.
///
/// # Errors
///
/// - [`Error::Io`] if the file cannot be opened or read
pub fn digest_file(path: &Path) -> Result<u128> {
    let file = std::fs::File::open(path).map_err(Error::io(path))?;
    let mut hasher = Xxh3::new();
    std::io::copy(&mut std::io::BufReader::new(file), &mut hasher).map_err(Error::io(path))?;
    Ok(hasher.digest128())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        write_formats(&scratch.path().join(FORMATS), &formats).unwrap();

        assert_eq!(read_formats(scratch.path()).unwrap(), formats);
    }
}
