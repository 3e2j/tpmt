//! A whole playable disc.
//!
//! Needs the whole source disc, since the image is that disc with the rebuilt
//! files swapped in.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use tpmt_disc::{Disc, Entry, Item, Layout, Span};
use tpmt_report::Step;

use crate::build::{Context, Job, Source, rebuild};
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
/// - [`Error::SourceChanged`] if it now holds another game or revision
/// - [`Error::Disc`] if the image will not lay out or will not write
/// - whatever assembling a changed file hit. See [`crate::build`]
pub fn write<E>(context: &Context<'_, E>, out: &Path) -> Result<PathBuf, E>
where
    E: From<Error> + Send,
{
    let job = context.job;
    let disc = open(&job.source)?;
    let staged = out.join(STAGING);
    rebuild(context, &staged)?;
    Ok(lay_out(job, &disc, &staged, &context.rebuilt, out)?)
}

/// Writes the image around the rebuilt files under `staged`, then clears
/// them.
fn lay_out<E>(
    job: &Job<'_, E>,
    disc: &Disc,
    staged: &Path,
    changed: &BTreeSet<String>,
    out: &Path,
) -> Result<PathBuf> {
    let original = disc.entries()?;
    let sources = sources(&original, staged, changed)?;
    let layout = Layout::plan(job.metadata, &items(&original, &sources))?;

    let name = format!("{}.iso", job.name);
    let path = out.join(&name);
    let mut image = layout.write(BufWriter::new(fs::create(&path)?));

    let writing = job
        .progress
        .begin(Step::WriteImage, sources.values().map(Bytes::size).sum());

    for entry in layout.entries() {
        let Entry::File { path: at, .. } = entry else {
            continue;
        };

        let Some(source) = sources.get(at.as_str()) else {
            return Err(Error::Unsourced(at.clone()));
        };
        let bytes = match source {
            Bytes::Disc(span) => disc.read(*span)?,
            Bytes::Staged { .. } => fs::read(&staged.join(at))?,
        };
        image.file(&bytes)?;
        writing.add(source.size());
    }
    image.finish()?;

    fs::remove_dir_all_if_exists(staged)?;
    Ok(PathBuf::from(name))
}

/// Where one file of the image gets its bytes.
enum Bytes {
    /// Untouched, so read straight off the source disc. The layout holds the
    /// offset it is going to, which is not this one.
    Disc(Span),
    /// Rebuilt, and waiting under the staging directory.
    Staged { size: u64 },
}

impl Bytes {
    const fn size(&self) -> u64 {
        match *self {
            Self::Disc(Span { size, .. }) | Self::Staged { size } => size,
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
        if let Entry::File { path, span } = entry {
            sources.insert(path.as_str(), Bytes::Disc(*span));
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

/// Opens the disc this project was unpacked from, and checks it still holds
/// the same game and revision.
///
/// Another dump of the same revision passes, since its unchanged files are
/// the same bytes. A disc edited in place under the same id passes too.
fn open(source: &Source<'_>) -> Result<Disc> {
    let disc = match Disc::open(source.iso) {
        Ok(disc) => disc,
        Err(tpmt_disc::Error::Open { source: io, .. })
            if io.kind() == std::io::ErrorKind::NotFound =>
        {
            return Err(Error::SourceMissing(source.iso.to_path_buf()));
        }
        Err(error) => return Err(error.into()),
    };
    let boot = &disc.metadata().boot;
    if !source.matches(boot) {
        return Err(Error::SourceChanged {
            iso: source.iso.to_path_buf(),
            unpacked: format!("{} revision {}", source.id, source.revision),
            found: format!("{} revision {}", boot.id, boot.revision),
        });
    }
    Ok(disc)
}
