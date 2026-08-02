//! Unpacking a disc to a project folder and building it back.
//!
//! Unpacking walks the disc, peels off compression, opens archives, hands decodable
//! files to whichever format crate claims them, and writes the result out as a single
//! folder a person edits in place. Building runs the same route backwards.
//!
//! Edits are never tracked as they happen. Which files a person touched is
//! worked out at build time, by hashing them against the vanilla hashes taken
//! at unpack, and only those get re-encoded. Everything else is copied out of
//! the source disc verbatim as raw bytes.
//!
//! Rebuilding an archive does not mean rebuilding what is inside it. An archive
//! nobody edited is copied off the disc whole, still compressed. An archive with
//! an edit in it gets repacked and recompressed, but only the files that were
//! actually edited are re-encoded; every other file in it keeps the exact bytes
//! it had on the disc. No file is ever put back through a codec because
//! something sitting next to it changed.
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
//   tpmt.toml   schema version, disc id + revision + sha1
//   sys/        boot.bin, bi2.bin, apploader.img, main.dol, fst.bin
//   files/      game content, archives as directories
//
// Store, everything we generate:
//   discs/GZ2E-rev0/source.toml   where the ISO was last seen, plus its sha1
//   discs/GZ2E-rev0/hashes        vanilla hashes, for change detection
//   out/                          built images
//
// Paths mirror the disc, decoded files chain extensions (zel_00.bmg.json).

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

use std::path::Path;

/// Unpacks a disc image into a new project directory.
///
/// The directory is created here and is expected not to exist yet. Reusing one
/// would mean reconciling whatever edits are already sitting in it, which is
/// what `build` is for.
pub fn unpack(iso: &Path, project: &Path) -> Result<(), Error> {
    let _ = (iso, project);
    Err(Error::Unimplemented)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unpacking is not implemented yet")]
    Unimplemented,
}
