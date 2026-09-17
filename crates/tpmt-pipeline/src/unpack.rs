//! Walks a disc, decodes whatever [`crate::format`] recognises, and lays it
//! out under `base/`. See [`crate::unpack`].

use std::collections::BTreeMap;
use std::path::Path;

use rayon::prelude::*;
use sha1::{Digest, Sha1};
use tpmt_disc::{Disc, Entry};

use crate::{Result, format, project};

pub fn run(iso: &Path, project: &Path) -> Result<()> {
    // Checked before the disc is even opened: whether this directory is safe
    // to write into does not depend on what is in the ISO.
    project::prepare(project)?;
    // Scaffolded alongside base/ rather than left for the first structured
    // edit to create, so a fresh project has somewhere for overlay/res/
    // edits to land immediately. A no-op if mod/ already exists.
    project::scaffold_mod(project)?;

    let base = project::base_staging_dir(project);
    let disc = Disc::open(iso)?;

    let hashes = match unpack_into(&disc, &base) {
        Ok(hashes) => hashes,
        Err(err) => {
            // Best-effort: the staging dir is cruft either way, but the
            // unpack error is the one that matters.
            let _ = project::clear_staging(project);
            return Err(err);
        }
    };

    project::promote_base(project)?;
    project::commit(project, iso, &disc.sha1()?, &hashes)
}

/// Writes one disc's worth of files into `base`, hashing each as it goes.
fn unpack_into(disc: &Disc, base: &Path) -> Result<BTreeMap<String, String>> {
    project::write_metadata(base, disc.metadata())?;

    Ok(disc
        .entries()?
        .par_iter()
        .map(|entry| unpack_entry(disc, base, entry))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|written| (written.path, written.sha1))
        .collect())
}

/// One file written into `base/`, and what it hashed to.
struct WrittenFile {
    path: String,
    sha1: String,
}

/// Decodes one disc entry into `base/`. A directory holds nothing to hash,
/// but is created here so an empty one is not lost.
fn unpack_entry(disc: &Disc, base: &Path, entry: &Entry) -> Result<Vec<WrittenFile>> {
    let Entry::File { path, offset, size } = entry else {
        project::create_dir_all(&base.join(entry.path()))?;
        return Ok(Vec::new());
    };

    let data = disc.read(*offset, *size)?;
    format::decode(path, &data)?
        .into_iter()
        .map(|(path, data)| {
            project::write(&base.join(&path), &data)?;
            Ok(WrittenFile {
                sha1: sha1_hex(&data),
                path,
            })
        })
        .collect()
}

fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
