//! A whole playable disc.
//!
//! Needs the whole source disc, since the image is that disc with the rebuilt
//! files swapped in.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use tpmt_disc::{Disc, Entry, Item, Layout};

use crate::build::{Job, rebuild};
use crate::project::metadata::Source;
use crate::{Error, Result, fs};

/// Where rebuilt disc files wait while the image is laid out around them.
/// Cleared again once the image is written, since the patch target is where
/// somebody goes for those files on their own.
const STAGING: &str = ".rebuilt";

/// Writes the image into `out`, named after the project, and returns that
/// name.
///
/// # Errors
///
/// - [`Error::SourceMissing`] if the disc this project came from has moved
/// - [`Error::SourceChanged`] if it is no longer the same dump
/// - [`Error::Disc`] if the image will not lay out or will not write
/// - whatever assembling a changed file hit. See [`crate::build::run`]
pub fn write(job: &Job, out: &Path) -> Result<PathBuf> {
    let disc = open(job.source)?;
    let staged = out.join(STAGING);
    rebuild(job, &staged)?;

    let original = disc.entries()?;
    let sources = sources(&original, &staged, job.changed)?;
    let layout = Layout::plan(&job.base.metadata, &items(&original, &sources))?;

    let name = name(job);
    let path = out.join(&name);
    let file = File::create(&path).map_err(fs::io_at(&path))?;
    let mut image = layout.write(BufWriter::new(file));

    for entry in layout.entries() {
        let Entry::File { path: at, .. } = entry else {
            continue;
        };

        let bytes = match sources.get(at.as_str()) {
            Some(Bytes::Disc { offset, size }) => disc.read(*offset, *size)?,
            Some(Bytes::Staged { .. }) => fs::read(&staged.join(at))?,
            None => return Err(Error::MissingFile(at.clone())),
        };
        image.file(&bytes)?;
    }
    image.finish()?;

    fs::remove_dir_all_if_exists(&staged)?;
    Ok(PathBuf::from(name))
}

/// Where one file of the image gets its bytes.
enum Bytes {
    /// Untouched, so read straight off the source disc. The layout holds the
    /// offset it is going to, which is not this one.
    Disc { offset: u64, size: u64 },
    /// Rebuilt, and waiting under the staging directory.
    Staged { size: u64 },
}

impl Bytes {
    const fn size(&self) -> u64 {
        match *self {
            Self::Disc { size, .. } | Self::Staged { size } => size,
        }
    }
}

/// Where every file of the image comes from, keyed by disc path.
///
/// Unchanged files take their offset and size from the source disc's file
/// table, which the `Tree` doesn't have. Rebuilt files take their size from
/// their staged copy.
fn sources<'a>(
    original: &'a [Entry],
    staged: &Path,
    changed: &'a BTreeSet<String>,
) -> Result<BTreeMap<&'a str, Bytes>> {
    let mut sources = BTreeMap::new();
    for entry in original {
        if let Entry::File { path, offset, size } = entry {
            let (offset, size) = (*offset, *size);
            sources.insert(path.as_str(), Bytes::Disc { offset, size });
        }
    }
    for path in changed {
        let size = fs::len(&staged.join(path))?;
        sources.insert(path.as_str(), Bytes::Staged { size });
    }
    Ok(sources)
}

/// Everything the image will hold. The file table sorts its own entries, so
/// the order here does not matter.
fn items(original: &[Entry], sources: &BTreeMap<&str, Bytes>) -> Vec<Item> {
    let directories = original.iter().filter_map(|entry| match entry {
        Entry::Directory { path } => Some(Item::Directory { path: path.clone() }),
        Entry::File { .. } => None,
    });
    let files = sources.iter().map(|(path, bytes)| Item::File {
        path: (*path).to_string(),
        size: bytes.size(),
    });
    directories.chain(files).collect()
}

/// Opens the disc this project was unpacked from, and checks it is still the
/// same one.
fn open(source: &Source) -> Result<Disc> {
    if !source.iso.is_file() {
        return Err(Error::SourceMissing(source.iso.clone()));
    }

    let disc = Disc::open(&source.iso)?;
    if disc.sha1()? != source.sha1 {
        return Err(Error::SourceChanged(source.iso.clone()));
    }
    Ok(disc)
}

/// What the image is called: the project's own name, or failing that the game
/// the disc says it is.
fn name(job: &Job) -> String {
    let stem = job
        .project
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(&job.base.metadata.boot.id);
    format!("{stem}.iso")
}
