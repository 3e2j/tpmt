//! Walks a disc, explodes each file (see [`explode`]), and lays the
//! result out under `base/`. See [`crate::unpack`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rayon::prelude::*;
use tpmt_disc::{Disc, Entry, Span};

use crate::progress::{Progress, Step};
use crate::{Result, fs, project};

pub mod explode;

/// Unpacks a disc into `base/`, records the store under `.tpmt/`, and
/// scaffolds a `mod/` folder.
pub fn run(iso: &Path, project: &Path, progress: &Progress) -> Result<()> {
    project::refuse_foreign(project)?;
    let disc = Disc::open(iso)?;
    let sha1 = project::metadata::sha1_disc(&disc, progress)?;

    let staging = project::Staging::begin(&project::base(project))?;
    let (yaz0_compressed, hashes) = unpack_files(&disc, staging.dir(), progress)?;

    progress.begin(Step::Save, 0);
    project::metadata::write_base(staging.dir(), disc.metadata(), yaz0_compressed)?;
    staging.promote()?;

    project::metadata::write_store(project, iso, &sha1, &hashes)?;

    project::scaffold_mod(project)
}

/// Unpacks one disc's worth of files into `base`, hashing each as it goes,
/// and returns what `base/`'s metadata needs to say about them.
fn unpack_files(
    disc: &Disc,
    base: &Path,
    progress: &Progress,
) -> Result<(BTreeSet<String>, BTreeMap<String, String>)> {
    // Create every listed directory before any file, so empty directories
    // survive the unpack.
    let entries = disc.entries()?;
    for entry in &entries {
        if let Entry::Directory { path } = entry {
            fs::create_dir_all(&base.join(path))?;
        }
    }

    let files: Vec<_> = entries
        .iter()
        .filter_map(|entry| Some((entry.path(), entry.span()?)))
        .collect();

    let unpacking = progress.begin(Step::Unpack, files.iter().map(|(_, span)| span.size).sum());
    let unpacked_files = files
        .into_par_iter()
        .map(|(path, span)| {
            let unpacked = unpack_file(disc, base, path, span)?;
            unpacking.add(span.size);
            Ok(unpacked)
        })
        .collect::<Result<Vec<_>>>()?;

    let yaz0_compressed = unpacked_files
        .iter()
        .filter(|file| file.yaz0_compressed)
        .map(|file| file.path.clone())
        .collect();

    let hashes = unpacked_files
        .into_iter()
        .flat_map(|file| file.hashes)
        .collect();

    Ok((yaz0_compressed, hashes))
}

/// One disc file laid out under `base/`.
struct Unpacked {
    /// The file's disc path.
    path: String,
    /// Whether a Yaz0 wrapper came off it. The disc is the container that
    /// records this for a loose file, in `yaz0.toml`.
    yaz0_compressed: bool,
    /// What every project file it became hashed to, keyed by project path.
    hashes: BTreeMap<String, String>,
}

/// Explodes one disc file into `base/`, hashing each project file as it
/// lands.
fn unpack_file(disc: &Disc, base: &Path, path: &str, span: Span) -> Result<Unpacked> {
    let data = disc.read(span)?;
    let mut hashes = BTreeMap::new();
    let yaz0_compressed = explode::file(path, &data, &mut |path, data| {
        fs::write(&base.join(path), data)?;
        hashes.insert(path.to_string(), project::metadata::sha1_hex(data));
        Ok(())
    })?;

    Ok(Unpacked {
        path: path.to_string(),
        yaz0_compressed,
        hashes,
    })
}
