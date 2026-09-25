//! Walks a disc, explodes each file (see [`explode`]), and lays the
//! result out under `base/`. See [`crate::unpack`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rayon::prelude::*;
use tpmt_disc::{Disc, Entry};

use crate::progress::{Progress, Step};
use crate::{Result, fs, project};

pub mod explode;

/// Unpacks a disc into `base/`, records the store under `.tpmt/`, and
/// scaffolds a `mod/` folder.
pub fn run(iso: &Path, project: &Path, progress: &Progress) -> Result<()> {
    project::refuse_foreign(project)?;
    let disc = Disc::open(iso)?;

    let staging = project::Staging::begin(&project::base(project))?;
    let Unpacked {
        sha1,
        yaz0_compressed,
        hashes,
    } = unpack_disc(&disc, staging.dir(), progress)?;

    progress.begin(Step::Save, 0);
    project::metadata::write_base(staging.dir(), disc.metadata(), yaz0_compressed)?;
    staging.promote()?;

    project::metadata::write_store(project, iso, &sha1, &hashes)?;

    project::scaffold_mod(project)
}

/// What one read of the disc leaves for the project to record.
struct Unpacked {
    /// The whole image's SHA-1, for `source.toml`.
    sha1: String,
    /// The disc files that arrived Yaz0 wrapped, for `yaz0.toml`.
    yaz0_compressed: BTreeSet<String>,
    /// What every project file hashed to, keyed by project path.
    hashes: BTreeMap<String, String>,
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

    let unpacking = progress.begin(Step::Unpack, disc.len());
    let mut stream = disc.stream(&entries, |size| unpacking.add(size));

    // Workers pull files off the stream one at a time, so the disc is still
    // read in order and at most one file per worker sits in memory.
    let unpacked_files = stream
        .by_ref()
        .par_bridge()
        .map(|file| {
            let (path, data) = file?;
            unpack_file(base, path, &data)
        })
        .collect::<Result<Vec<_>>>()?;
    let sha1 = stream.finish()?;

    let yaz0_compressed = unpacked_files
        .iter()
        .filter(|file| file.yaz0_compressed)
        .map(|file| file.path.clone())
        .collect();

    let hashes = unpacked_files
        .into_iter()
        .flat_map(|file| file.hashes)
        .collect();

    Ok(Unpacked {
        sha1,
        yaz0_compressed,
        hashes,
    })
}

/// One disc file laid out under `base/`.
struct UnpackedFile {
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
fn unpack_file(base: &Path, path: &str, data: &[u8]) -> Result<UnpackedFile> {
    let mut hashes = BTreeMap::new();
    let yaz0_compressed = explode::file(path, data, &mut |path, data| {
        fs::write(&base.join(path), data)?;
        hashes.insert(path.to_string(), project::metadata::sha1_hex(data));
        Ok(())
    })?;

    Ok(UnpackedFile {
        path: path.to_string(),
        yaz0_compressed,
        hashes,
    })
}
