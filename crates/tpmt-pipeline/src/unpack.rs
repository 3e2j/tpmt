//! Walks a disc, explodes each file (see [`crate::explode`]), and lays the
//! result out under `base/`. See [`crate::unpack`].

use std::collections::BTreeMap;
use std::path::Path;

use rayon::prelude::*;
use sha1::{Digest, Sha1};
use tpmt_disc::{Disc, Entry};

use crate::{Result, explode, project};

/// Every fallible step comes before every irreversible one: nothing under
/// `project` changes until the whole disc has been read and hashed.
pub fn run(iso: &Path, project: &Path) -> Result<()> {
    // Checked before the disc is even opened: whether this directory is safe
    // to write into does not depend on what is in the ISO.
    project::refuse_foreign(project)?;
    let disc = Disc::open(iso)?;

    let staging = project::Staging::begin(project)?;
    let hashes = unpack_into(&disc, staging.dir())?;
    let sha1 = disc.sha1()?;

    staging.promote()?;
    project::commit(project, iso, &sha1, &hashes)?;
    // Scaffolded alongside base/ rather than left for the first structured
    // edit to create, so a fresh project has somewhere for overlay/res/
    // edits to land immediately. A no-op if mod/ already exists.
    project::scaffold_mod(project)
}

/// Writes one disc's worth of files into `base`, hashing each as it goes.
fn unpack_into(disc: &Disc, base: &Path) -> Result<BTreeMap<String, String>> {
    project::write_metadata(base, disc.metadata())?;

    // Directories hold nothing to hash, but an empty one would otherwise be
    // lost, since a file only creates the directories on its own path.
    let entries = disc.entries()?;
    for entry in &entries {
        if let Entry::Directory { path } = entry {
            project::create_dir_all(&base.join(path))?;
        }
    }

    let unpacked = entries
        .par_iter()
        .filter_map(|entry| match entry {
            Entry::File { path, offset, size } => Some((path, *offset, *size)),
            Entry::Directory { .. } => None,
        })
        .map(|(path, offset, size)| unpack_file(disc, base, path, offset, size))
        .collect::<Result<Vec<_>>>()?;

    let yaz0_compressed: Vec<_> = unpacked
        .iter()
        .filter(|file| file.yaz0_compressed)
        .map(|file| file.path.clone())
        .collect();
    project::write_yaz0(base, &yaz0_compressed)?;

    Ok(unpacked.into_iter().flat_map(|file| file.written).collect())
}

/// One disc file laid out under `base/`.
struct Unpacked {
    /// The file's disc path.
    path: String,
    /// Whether a Yaz0 wrapper came off it. The disc is the container that
    /// records this for a loose file, in `yaz0.toml`.
    yaz0_compressed: bool,
    /// Every project file it became, and what each hashed to.
    written: Vec<(String, String)>,
}

/// Explodes one disc file into `base/`, hashing each project file as it
/// lands.
fn unpack_file(disc: &Disc, base: &Path, path: &str, offset: u64, size: u64) -> Result<Unpacked> {
    let data = disc.read(offset, size)?;
    let mut written = Vec::new();
    let yaz0_compressed = explode::file(path, &data, &mut |path, data| {
        project::write(&base.join(path), data)?;
        written.push((path.to_string(), sha1_hex(data)));
        Ok(())
    })?;

    Ok(Unpacked {
        path: path.to_string(),
        yaz0_compressed,
        written,
    })
}

fn sha1_hex(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
