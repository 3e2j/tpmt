//! Unpacking a disc to a project folder and building it back.
//!
//! Unpacking walks the disc, peels off compression, opens archives, hands decodable
//! files to whichever format crate claims them, and writes the result out as a single
//! folder a person edits in place. Building runs the same route backwards.
//!
//! Building has two endings. A build works out what changed, re-encodes it, and
//! gathers the finished game files into a mod somebody else can install. An
//! image carries on from there and lays those files out as a disc, which is the
//! only step of the two that has to know where anything goes. Everything before
//! that split is one path, and neither ending is built by way of the other.
//!
//! Edits are never tracked as they happen. Which files a person touched is
//! worked out at build time, by hashing them against the vanilla hashes taken
//! at unpack, and only those get re-encoded. Everything else is copied out of
//! the source disc verbatim as raw bytes.
//!
//! Rebuilding an archive does not mean rebuilding what is inside it. An archive
//! nobody touched is copied off the disc whole, still compressed. An archive
//! counts as modified when any member was edited, added or deleted, and only
//! then is it repacked and recompressed. Even so, only the added and edited
//! members are re-encoded: every member left alone is pulled verbatim off the
//! source disc rather than rebuilt, and a deleted member is simply not pulled
//! at all. No file is ever put back through a codec because something sitting
//! next to it changed.
//!
//! The only crate allowed to know how formats stack. That a `.arc` on the disc
//! is usually Yaz0 wrapped around RARC is a fact about this pipeline, not about
//! either format.
//!
//! Format crates convert their own file type to an editable form and back.
//! Which form that is belongs to the crate rather than here, so this one only
//! asks whether a conversion exists and takes back what it produces, extension
//! included. Their typed models are public as well, reachable by an editor
//! without a file in between.
//!
//! Unpack fans out one flat layer over FST entries, each self-contained.

// Project, one directory, edited in place:
//   tpmt.toml   schema version, the preamble values a build cannot derive
//   sys/        apploader.img, main.dol
//   files/      game content, archives as directories
//
// Store, everything we generate:
//   discs/GZ2E-rev0/source.toml   where the ISO was last seen, plus its sha1
//   discs/GZ2E-rev0/hashes        vanilla hashes, for change detection
//   out/                          built mods and images
//
// Paths mirror the disc, decoded files chain extensions (zel_00.bmg.json).

// TODO: build and image. Nothing goes back the other way yet, this crate only
// unpacks. Everything above about change detection, selective re-encoding and
// repacking archives is where that is going rather than what is here. Repacking
// with an unchanged member set is what `tpmt_arc::build` already does; adding or
// removing members waits on the re-emitter marked TODO in tpmt-arc.

// TODO: the golden roundtrip. The end to end fidelity test: unpack a retail
// ISO, image it straight back with nothing edited, and require every entry the
// disc reports to come back byte for byte. It lives at this level because only
// the pipeline sees the whole path (disc, compression, archives, formats); the
// format crates each proved their own fidelity with throwaway probes, and this
// is the committed test that keeps all of it true at once. Needs an ISO on hand,
// so it runs as an ignored integration test under tests/ pointed at assets/.
// Blocked on build and image existing at all. It goes through image, since that
// is the one that produces something a disc reader can be pointed back at, and
// getting there means every step build owns was already right.
//
// It compares entries, not whole files: an image drops the mastering fill (see
// tpmt-disc) and so is smaller than its source. assets/NA.iso is already
// scrubbed, so it is the one print where comparing the files would pass anyway.
// Do not calibrate on it.
//
// The file table is the exception worth checking whole. It is never unpacked,
// so an image derives it from the project tree, and the source disc still has
// the original to hold that against. Only the offsets are allowed to differ.

// TODO: the layout above is the target, not what unpack writes. Today it makes
// tpmt.toml, sys/ and files/ and stops: no store, no out/. The project file is
// missing the disc's sha1, which waits on something computing one.

// TODO: decide whether to keep our own copy of the ISO rather than remembering
// its originating path. Either way, tell the user before taking their disk space.

// TODO: routing tables (disc path to project path) are still hardcoded nowhere.
// Scope is Twilight Princess only, but GZ2E, GZ2P and GZ2J do not share paths,
// so whatever holds them is keyed by region.

// TODO: scratch space. Half-written output is never left where a person can
// mistake it for a finished one, so anything generated is staged and then
// renamed into place. Staging goes inside the store rather than the platform
// temp directory: a rename is only atomic within one filesystem, and on Windows
// it fails outright across volumes, which is exactly where a temp directory
// tends to sit relative to a project.

// TODO: the vanilla hashes, and the tpmt.toml that points at them. Build needs
// both to tell an edited file from an untouched one, and nothing builds yet.

// TODO: hand files to the format crates. Every file currently lands as the
// bytes the disc holds, so a `.bmg` is still a `.bmg` rather than the JSON a
// person would edit.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tpmt_disc::{Bi2, Boot, Disc, Entry};

/// The three Twilight Princess prints. Any other disc would unpack fine and
/// then mean nothing to the rest of the toolkit, so it is turned away here.
const SUPPORTED_IDS: [&str; 3] = ["GZ2E", "GZ2P", "GZ2J"];

/// Bumped whenever a field here moves or changes meaning, so an old project is
/// told so rather than misread.
const SCHEMA: u32 = 1;

/// The project file: what a build needs that is not a file in the project.
///
/// The disc's boot header and metadata are nine kilobytes of mostly nothing, so
/// they are kept as their values instead, which also puts the title and the
/// region somewhere a person can edit them.
#[derive(serde::Serialize)]
struct Project<'a> {
    schema: u32,
    boot: &'a Boot,
    bi2: &'a Bi2,
}

/// Unpacks a disc image into a new project directory.
///
/// The directory is created here and is expected not to exist yet. Reusing one
/// would mean reconciling whatever edits are already sitting in it, which is
/// what `build` is for.
pub fn unpack(iso: &Path, project: &Path) -> Result<(), Error> {
    let disc = Disc::open(iso)?;
    let metadata = disc.metadata();
    let id = &metadata.boot.id;
    if !SUPPORTED_IDS.contains(&id.as_str()) {
        return Err(Error::UnsupportedDisc(id.clone()));
    }

    if project.exists() {
        return Err(Error::ProjectExists(project.to_path_buf()));
    }
    create_dir(project)?;

    let file = Project {
        schema: SCHEMA,
        boot: &metadata.boot,
        bi2: &metadata.bi2,
    };
    write(
        &project.join("tpmt.toml"),
        toml::to_string_pretty(&file)?.as_bytes(),
    )?;

    // One flat layer of work. Every entry reads its own bytes off the shared
    // disc and writes its own outputs, so there is nothing to hand between
    // threads and nothing to order them by.
    disc.entries()?
        .par_iter()
        .try_for_each(|entry| unpack_entry(&disc, entry, project))
}

/// Unpacks one disc entry: an archive becomes a directory of its contents,
/// anything else becomes a file of the bytes the disc holds.
///
/// A directory entry only matters when it is empty. Anything with members gets
/// created on the way past by the members themselves.
fn unpack_entry(disc: &Disc, entry: &Entry, project: &Path) -> Result<(), Error> {
    let path = project.join(entry.path());
    let Entry::File { offset, size, .. } = *entry else {
        return create_dir(&path);
    };

    let bytes = disc.read(offset, size)?;
    match is_archive(entry.path()) {
        true => unpack_archive(&bytes, &path),
        false => write(&path, &bytes),
    }
}

/// Unpacks an archive into a directory named after it.
///
/// Keeping the `.arc` on the directory is what lets build recognise it later
/// without a note anywhere saying so.
///
/// That an archive on this disc is Yaz0 wrapped around RARC is known here and
/// nowhere else: the compression is a fact about where the file sits, not
/// something either format has an opinion about. Archives nested inside
/// archives are wrapped the same way and unpack the same way.
///
/// Not every `.arc` is one of ours. A handful hold other container formats
/// entirely, and those stay whole, exactly as the disc had them, rather than
/// being half-understood.
fn unpack_archive(bytes: &[u8], path: &Path) -> Result<(), Error> {
    let contents = match tpmt_compress::is_yaz0(bytes) {
        true => {
            Cow::Owned(
                tpmt_compress::yaz0_decode(bytes).map_err(|source| Error::Compress {
                    path: path.to_path_buf(),
                    source,
                })?,
            )
        }
        false => Cow::Borrowed(bytes),
    };

    let files = match tpmt_arc::files(&contents) {
        Ok(files) => files,
        // The original bytes, not the decompressed ones: what is written here
        // has to be what goes back on the disc.
        Err(tpmt_arc::Error::NotRarc) => return write(path, bytes),
        Err(source) => {
            return Err(Error::Archive {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    create_dir(path)?;
    for file in files {
        let path = path.join(&file.path);
        match is_archive(&file.path) {
            true => unpack_archive(file.data, &path)?,
            false => write(&path, file.data)?,
        }
    }

    Ok(())
}

fn is_archive(path: &str) -> bool {
    path.ends_with(".arc")
}

fn create_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    fs::write(path, bytes).map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not a Twilight Princess disc")]
    UnsupportedDisc(String),

    #[error("`{}` already exists, so there is nothing to unpack into", .0.display())]
    ProjectExists(PathBuf),

    #[error("`{}`: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },

    #[error("`{}`: {source}", .path.display())]
    Compress {
        path: PathBuf,
        source: tpmt_compress::Error,
    },

    #[error("`{}`: {source}", .path.display())]
    Archive {
        path: PathBuf,
        source: tpmt_arc::Error,
    },

    #[error("the project file could not be written: {0}")]
    Project(#[from] toml::ser::Error),

    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),
}
