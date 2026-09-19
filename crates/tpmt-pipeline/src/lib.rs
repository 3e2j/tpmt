//! Unpacking a disc to a project folder, and building it back. This is the
//! convayer-belt assembly line that interacts with all game formats.
//! The `tpmt` CLI is a thin wrapper around the logic here.
//!
//! A format crate owns its own conversion to and from an editable form:
//! this crate only hands those format crate's the relevant data blobs.
//!
//! Project layout: see `project.rs`.

// Rewrite in progress. The previous implementation is gone from the working
// tree but not from history: `git log` on this branch (pipeline-rewrite) has
// it as of 7ff78a1, for reference while rebuilding.

// TODO: golden roundtrip tests. Unpack a retail ISO, image it straight back
// with nothing edited, and diff the two.
//
// Byte equality is the wrong measurement for rebuilt entries. A format that
// stores something derived comes back derived rather than restored, so the
// bytes can move freely without anything being "wrong".

// TODO: routing tables (disc path to project path) are still hardcoded nowhere.
// Scope is Twilight Princess only, but GZ2E, GZ2P and GZ2J do not share paths,
// so whatever holds them is keyed by region.

// TODO: a mod has no way to say a file was deleted, only which ones it
// replaces or adds. Only matters outside an archive, since a deleted member
// is already covered by the whole container repacking. An image handles a
// deletion fine either way, since it lays out whatever the tree holds. But
// a `build` folder can't.

// TODO: nothing here catches a deleted file that something else still
// references by id, path, or name; that only surfaces as a crash in game,
// far from the build that caused it. Two separate defenses belong here
// eventually: a build-time warning when a file the original archive held is
// gone from what gets packed (cheap, catches the common case, blind to
// whether anything actually references it), and the linker below.

// TODO: the linker. Lands with its first user (`.stb`), as a trait in
// tpmt-arc a decoded file implements to hand out `&mut` to every reference
// it holds, each an enum of bare `Id(u16)` or resolved `Path(String)`.
// Unpack turns `Id`s into `Path`s via the owning archive's id -> path map
// (free from `Sidecar::members`); build turns `Path`s back into ids once
// that archive's member list is fixed, before the referencer's bytes get
// encoded, no cycle since id assignment never depends on referencer
// content. Per-archive scope confirmed against the decomp
// (`JKRArchive::getResource`/`findIdResource`): refs never leave their
// archive.

use std::path::{Path, PathBuf};

mod explode;
mod project;
mod unpack;

pub use explode::DecodeError;
pub use project::{discover, is_project};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),

    /// A format crate rejected one file, named by its project path so a
    /// member of a nested archive points at itself.
    #[error("`{path}`: {source}")]
    Decode { path: String, source: DecodeError },

    #[error("`{}`: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("`{}` holds something this did not write, so it will not be replaced", .0.display())]
    ForeignDirectory(PathBuf),

    /// A file this crate generates (`disc.toml`, `mod.json`, the store)
    /// would not serialize.
    #[error("`{}` could not be written: {source}", .path.display())]
    Serialize {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("`{}` is not inside a project (no `.tpmt` found above it)", .0.display())]
    NoProjectFound(PathBuf),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// Walks the disc, peels off compression, opens archives, and hands each
/// file to whichever format crate can decode it, writing the result out as
/// `base/`.
///
/// `mod/` is scaffolded alongside it, empty, ready for the edits that make
/// it a mod.
///
/// `project` may be an existing project (marked for being overridden).
/// If it already exists, only `base/` is replaced; `mod/` is left alone.
/// Committed only once every file is derived, so a failure part way
/// through leaves no half-made project behind.
///
/// # Errors
///
/// - [`Error::ForeignDirectory`] if `project` holds something else
/// - [`Error::Disc`] if the ISO can't be opened or read
/// - [`Error::Decode`] if a file on it isn't what its bytes claim
/// - [`Error::Io`] on any write
pub fn unpack(iso: &Path, project: &Path) -> Result<(), Error> {
    unpack::run(iso, project)
}

/// Hashes the project tree against the vanilla hashes taken at [`unpack`],
/// and reports whatever doesn't match.
///
/// # Errors
///
/// Not yet designed.
pub fn status(project: &Path) -> Result<Vec<Change>, Error> {
    todo!()
}

/// Does the same hashing as [`status`], then re-encodes what changed and
/// copies everything else out of the source disc untouched.
///
/// # Errors
///
/// Not yet designed.
pub fn build(project: &Path, output: Option<&Path>) -> Result<PathBuf, Error> {
    todo!()
}

/// Lays a [`build`]'s files onto a disc, the one step that has to know where
/// anything goes.
///
/// # Errors
///
/// Not yet designed.
pub fn image(project: &Path, output: Option<&Path>) -> Result<PathBuf, Error> {
    todo!()
}

/// Puts an edited file back to its vanilla bytes, re-unpacking its archive
/// fresh when it lives inside one.
///
/// # Errors
///
/// Not yet designed.
pub fn revert(project: &Path, target: &Path) -> Result<(), Error> {
    todo!()
}
