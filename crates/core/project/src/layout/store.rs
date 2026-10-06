//! `.tpmt/`: everything an unpack records about the project rather than for
//! it, one type per file:
//!
//! ```text
//! source.toml      Source    where the game image was last seen, and which game
//! digests.xxh128   Digests   digest of every vanilla/ file
//! payloads         Payloads  which vanilla/ files hold each payload
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
use crate::{Error, FileKind, Payload, Result};

pub const DIR: &str = ".tpmt";
const PAYLOADS: &str = "payloads";
const DIGESTS: &str = "digests.xxh128";
const SOURCE_TOML: &str = "source.toml";

/// `source.toml`: where the game image this project came from was last seen, and
/// the game id and revision it held, so a build can tell if it moved or now
/// holds another version.
///
/// Kept apart from `disc.toml`, which a modder may edit to rename the build.
#[derive(Serialize, Deserialize)]
pub struct Source {
    pub game_image: PathBuf,
    pub id: String,
    pub revision: u8,
}

/// `source.toml` and `digests`, which a build and a diff read together.
/// `payloads` isn't included, so looking up files by payload never parses
/// the digest of every `vanilla/` file.
pub struct Store {
    pub source: Source,
    pub digests: Digests,
}

pub fn write(
    dir: &Path,
    game_image: &Path,
    id: &str,
    revision: u8,
    digests: &Digests,
    payloads: &Payloads,
) -> Result<()> {
    let game_image = game_image.canonicalize().map_err(Error::io(game_image))?;
    write_digests(&dir.join(DIGESTS), digests)?;
    write_payloads(&dir.join(PAYLOADS), payloads)?;
    write_toml(
        &dir.join(SOURCE_TOML),
        &Source {
            game_image,
            id: id.to_string(),
            revision,
        },
    )
}

pub fn read(dir: &Path) -> Result<Store> {
    Ok(Store {
        source: read_toml(&dir.join(SOURCE_TOML))?,
        digests: read_digests(&dir.join(DIGESTS))?,
    })
}

/// `digests`: the vanilla XXH3-128 digest of every project file, keyed by project
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

/// `payloads`: every `vanilla/` file whose magic names a [`Payload`],
/// grouped by it. Any other file isn't listed.
///
/// On disk each kind is a `[name]` line with its paths one per line under
/// it, sorted by kind and then path.
///
/// It exists because a name on the disc can't be trusted: some files carry
/// no extension, or one that doesn't match what's inside. Only the magic
/// can. Unpack already holds every file's bytes, so it reads each magic once
/// and records it here, and a lookup by kind never reopens `vanilla/`.
pub type Payloads = BTreeMap<Payload, BTreeSet<String>>;

fn write_payloads(path: &Path, payloads: &Payloads) -> Result<()> {
    let mut text = String::new();
    for (payload, files) in payloads {
        text.push('[');
        text.push_str(payload.kind().name());
        text.push_str("]\n");
        for file in files {
            text.push_str(one_line(file)?);
            text.push('\n');
        }
    }
    fs::write(path, text.as_bytes())
}

pub fn read_payloads(dir: &Path) -> Result<Payloads> {
    let path = dir.join(PAYLOADS);
    let mut payloads = Payloads::new();
    let mut files = None;
    for line in read_text(&path)?.lines() {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let payload = FileKind::from_name(name)
                .and_then(FileKind::payload)
                .ok_or_else(|| Error::parse(&path)(format!("no payload is named `{name}`")))?;
            files = Some(payloads.entry(payload).or_default());
        } else {
            files
                .as_mut()
                .ok_or_else(|| Error::parse(&path)(format!("`{line}` comes before any `[kind]`")))?
                .insert(line.to_string());
        }
    }
    Ok(payloads)
}

/// `file`, if it reads back as itself from a line of its own. A newline would
/// split it in two, and a leading `[` would read as a `payloads` group.
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
/// every file in `changes/`, videos included.
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
    fn payloads_read_back_as_written() {
        let scratch = tempfile::tempdir().unwrap();
        let payloads = Payloads::from([(
            Payload::Mesg,
            BTreeSet::from(["files/a.arc/b.bmg".to_string(), "files/c.bmg".to_string()]),
        )]);
        write_payloads(&scratch.path().join(PAYLOADS), &payloads).unwrap();

        assert_eq!(read_payloads(scratch.path()).unwrap(), payloads);
    }
}
