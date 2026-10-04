//! Walks a disc and explodes each file (see [`explode`]), handing every leaf
//! to the caller to store. See [`crate::unpack`].

use std::collections::BTreeMap;
use std::path::Path;

use rayon::prelude::*;
use tpmt_binary::{Compression, FileKind};
use tpmt_disc::{Disc, Entry, Metadata};
use tpmt_report::{Progress, Step};

use crate::Error;

pub mod explode;

/// One file a project stores: a plain file, an archive member, or an
/// archive's sidecar. Archives and compression wrappers never arrive as one.
#[derive(Debug, Clone, Copy)]
pub struct Leaf<'a> {
    /// Its project path.
    pub path: &'a str,
    /// What its magic says it is, if anything.
    pub kind: Option<FileKind>,
    pub bytes: &'a [u8],
}

/// What a disc holds besides its leaves, and what `store` made of each leaf.
pub struct Unpacked<T> {
    /// The preamble values a build cannot derive.
    pub metadata: Metadata,
    /// Every directory the disc lists, so an empty one survives.
    pub directories: Vec<String>,
    /// The disc files that arrived compressed, and with which wrapper.
    pub compressed: BTreeMap<String, Compression>,
    /// What `store` returned for each leaf, in the order files finish.
    pub stored: Vec<T>,
}

/// Reads the disc once, in order, and calls `store` with every leaf. Every
/// drive handles that pattern well, and nothing relies on the page cache
/// holding the disc for a second pass.
///
/// Reports [`Step::Unpack`] across the whole image. Each file's reports go
/// to `progress` as soon as that file is walked.
///
/// `store` runs on several threads at once, one disc file per thread.
pub fn run<T, E>(
    iso: &Path,
    progress: &Progress,
    store: impl Fn(Leaf<'_>) -> Result<T, E> + Sync,
) -> Result<Unpacked<T>, E>
where
    T: Send,
    E: From<Error> + Send,
{
    let disc = Disc::open(iso).map_err(Error::from)?;
    let entries = disc.entries().map_err(Error::from)?;

    let mut directories = Vec::new();
    let mut total = 0;
    for entry in &entries {
        match entry {
            Entry::Directory { path } => directories.push(path.clone()),
            Entry::File { span, .. } => total += span.size,
        }
    }
    let unpacking = progress.begin(Step::Unpack, total);

    // Workers pull files off the stream one at a time, so the disc is still
    // read in order and at most one file per worker sits in memory.
    let files = disc
        .stream(&entries)
        .par_bridge()
        .map(|file| {
            let (path, data) = file.map_err(Error::from)?;
            unpacking.add(data.len() as u64);

            let mut stored = Vec::new();
            let mut reports = Vec::new();
            let compression = explode::file(
                path,
                &data,
                &mut |layer| -> Result<(), E> {
                    if layer.leaf {
                        stored.push(store(Leaf {
                            path: layer.path,
                            kind: layer.kind,
                            bytes: layer.bytes,
                        })?);
                    }
                    Ok(())
                },
                &mut reports,
            )?;
            progress.report(reports);
            Ok((
                compression.map(|compression| (path.to_string(), compression)),
                stored,
            ))
        })
        .collect::<Result<Vec<_>, E>>()?;

    let mut compressed = BTreeMap::new();
    let mut stored = Vec::new();
    for (wrapped, leaves) in files {
        compressed.extend(wrapped);
        stored.extend(leaves);
    }

    Ok(Unpacked {
        metadata: disc.metadata().clone(),
        directories,
        compressed,
        stored,
    })
}
