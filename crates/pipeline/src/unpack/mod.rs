//! Walks a disc, explodes each file (see [`explode`]), and lays the
//! result out under `base/`. See [`crate::unpack`].

use std::collections::BTreeSet;
use std::path::Path;

use rayon::prelude::*;
use tpmt_disc::{Disc, Entry};
use tpmt_project::io::{Staging, fs};
use tpmt_project::store::{Digests, Formats, digest};
use tpmt_project::{FileKind, Project, base};
use tpmt_report::Report;

use crate::Result;
use crate::progress::{Progress, Step};

pub mod explode;

/// Unpacks a disc into `base/`, records the store under `.tpmt/`, and
/// scaffolds a `mod/` folder. Returns the unpack's reports.
pub fn run(iso: &Path, project: &Project, progress: &Progress) -> Result<Vec<Report>> {
    let disc = Disc::open(iso)?;

    let staging = Staging::begin(&project.base())?;
    let Unpacked {
        yaz0_compressed,
        digests,
        formats,
        reports,
    } = unpack_disc(&disc, staging.dir(), progress)?;

    progress.begin(Step::Save, 0);
    base::write(staging.dir(), disc.metadata(), yaz0_compressed)?;
    staging.promote()?;

    project.write_store(iso, &disc.metadata().boot, &digests, &formats)?;

    project.scaffold_mod()?;
    Ok(reports)
}

/// What one read of the disc leaves for the project to record.
struct Unpacked {
    /// The disc files that arrived Yaz0 wrapped, for `yaz0.toml`.
    yaz0_compressed: BTreeSet<String>,
    /// What every project file hashed to, keyed by project path.
    digests: Digests,
    /// Which project files hold a known leaf format, for `.tpmt/formats`.
    formats: Formats,
    /// Every file's reports, in disc order.
    reports: Vec<Report>,
}

/// Unpacks one disc's worth of files into `base`, reading the disc once, in
/// order. Every drive handles that pattern well, and nothing relies on the
/// page cache holding the disc for a second pass.
fn unpack_disc(disc: &Disc, base: &Path, progress: &Progress) -> Result<Unpacked> {
    // Create every listed directory before any file, so empty directories
    // survive the unpack.
    let entries = disc.entries()?;
    for entry in &entries {
        if let Entry::Directory { path } = entry {
            fs::create_dir_all(&base.join(path))?;
        }
    }

    let total = entries.iter().filter_map(Entry::span).map(|span| span.size);
    let unpacking = progress.begin(Step::Unpack, total.sum());

    // Workers pull files off the stream one at a time, so the disc is still
    // read in order and at most one file per worker sits in memory.
    let unpacked_files = disc
        .stream(&entries)
        .par_bridge()
        .map(|file| {
            let (path, data) = file?;
            unpacking.add(data.len() as u64);
            unpack_file(base, path, &data)
        })
        .collect::<Result<Vec<_>>>()?;

    let yaz0_compressed = unpacked_files
        .iter()
        .filter(|file| file.yaz0_compressed)
        .map(|file| file.path.clone())
        .collect();

    let mut digests = Digests::new();
    let mut formats = Formats::new();
    let mut reports = Vec::new();
    for file in unpacked_files {
        digests.extend(file.digests);
        reports.extend(file.reports);
        for (kind, path) in file.kinds {
            formats.entry(kind).or_default().insert(path);
        }
    }

    Ok(Unpacked {
        yaz0_compressed,
        digests,
        formats,
        reports,
    })
}

/// One disc file laid out under `base/`.
struct UnpackedFile {
    /// The file's disc path.
    path: String,
    /// Whether a Yaz0 wrapper came off it. The disc is the container that
    /// records this for a loose file, in `yaz0.toml`.
    yaz0_compressed: bool,
    /// What every project file it became hashed to, in the order it landed.
    digests: Vec<(String, u128)>,
    /// The project files it became that hold a known leaf format.
    kinds: Vec<(FileKind, String)>,
    reports: Vec<Report>,
}

/// Explodes one disc file into `base/`, hashing and identifying each project
/// file as it lands.
fn unpack_file(base: &Path, path: &str, data: &[u8]) -> Result<UnpackedFile> {
    let mut digests = Vec::new();
    let mut kinds = Vec::new();
    let mut reports = Vec::new();
    let yaz0_compressed = explode::file(
        path,
        data,
        &mut |layer| {
            if !layer.leaf {
                return Ok(());
            }
            let path = layer.path;
            fs::write(&base.join(path), layer.bytes)?;
            digests.push((path.to_string(), digest(layer.bytes)));
            if let Some(kind) = layer.kind {
                kinds.push((kind, path.to_string()));
            }
            Ok(())
        },
        &mut reports,
    )?;

    Ok(UnpackedFile {
        path: path.to_string(),
        yaz0_compressed,
        digests,
        kinds,
        reports,
    })
}
