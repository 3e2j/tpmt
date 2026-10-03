//! Unpacking a disc to a project folder, and building it back. The `tpmt`
//! CLI is a thin wrapper around this crate.
//!
//! Unpack and build only handle containers: compression and archives.
//! A leaf format (BMG, ...) passes through both as raw bytes. Unpack sniffs
//! each file's magic to record its kind in `.tpmt/formats`, but never
//! decodes it.
//!
//! Everything after the unpack goes through a [`Project`], found with
//! [`Project::discover`] or returned by [`unpack`].
//!
//! A caller edits a leaf through three calls, none of which decode it.
//! [`Project::formats`] lists every leaf by kind. [`Project::read`] returns
//! one leaf's bytes, the `mod/overlay/` copy when there is one and the `base/`
//! copy otherwise. [`Project::write`] puts new bytes in `mod/overlay/`. The
//! caller hands the bytes to `tpmt-documents`, which decodes them.
//!
//! This crate owns what it takes to get from a disc to a project and back:
//! the disc image, archives, compression, and anything that spans files, like
//! cross-references. A leaf's own layout belongs to its format crate.
//!
//! Project layout: see `project/mod.rs`.

// TODO: a mod has no way to say a file was deleted, only which ones it
// replaces or adds. Only matters outside an archive, since a deleted member
// is already covered by the whole container repacking. An image handles a
// deletion fine either way, since it lays out whatever the tree holds. But
// a `build` folder can't.

// TODO: nothing here catches a deleted file that something else still
// references by id, path, or name; that only surfaces as a crash in game,
// far from the build that caused it. Two checks belong here eventually. One
// is a build-time warning when a file the original archive held is gone from
// what gets packed (cheap, catches the common case, blind to whether anything
// references it). The other is the linker (see `build::implode::archive`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tpmt_report::Report;

mod build;
mod fs;
mod leaf;
mod progress;
mod project;
mod status;
#[cfg(test)]
mod test_support;
mod unpack;

pub use build::{Built, EncodeError, Target};
pub use progress::{Progress, Snapshot, Step, Unit};
pub use project::is_project;
pub use tpmt_binary::FileKind;
pub use unpack::explode::{DecodeError, Layer, file as explode};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),

    /// A format crate rejected one file, named by its project path so a
    /// member of a nested archive points at itself.
    #[error("`{path}`: {source}")]
    Decode { path: String, source: DecodeError },

    /// A format crate would not write one file back out, named the same way.
    #[error("`{path}`: {source}")]
    Encode { path: String, source: EncodeError },

    #[error("`{}`: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("`{}` holds something tpmt did not write, so tpmt will not replace it", .0.display())]
    ForeignDirectory(PathBuf),

    /// A file this crate generates (`disc.toml`, `mod.json`, the store)
    /// would not serialize.
    #[error("could not serialize `{}`: {source}", .path.display())]
    Serialize {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A file this crate wrote will not read back as what it was.
    #[error("could not read `{}`: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("`{}` is not inside a project (no `.tpmt` found above it)", .0.display())]
    NoProjectFound(PathBuf),

    #[error("`{}` is not a name a project path can hold", .0.display())]
    UnusablePath(PathBuf),

    /// A vanilla file is no longer what the unpack recorded, so a build off
    /// it would pack somebody's edit as though the disc had shipped it.
    #[error("`{0}` in `base/` is not what was unpacked; re-unpack the disc, or put it back")]
    BaseModified(String),

    #[error("nothing in `base/` or `mod/overlay/` holds `{0}`")]
    MissingFile(String),

    #[error("the disc this project was unpacked from is no longer at `{}`", .0.display())]
    SourceMissing(PathBuf),

    /// The source disc now holds another version. Its unchanged files would
    /// not match what `base/` and the overlay were made against.
    #[error("`{}` holds {found}, but this project was unpacked from {unpacked}", .iso.display())]
    SourceChanged {
        iso: PathBuf,
        unpacked: String,
        found: String,
    },

    #[error("the {0} target is not implemented yet")]
    Unsupported(Target),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One file that differs from vanilla.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    /// A project path, under `mod/overlay/`.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
}

/// Walks the disc, peels off compression, opens archives, and hands each
/// file to whichever format crate can decode it, writing the result out as
/// `base/`. Returns the project it made.
///
/// Also scaffolds an empty `mod/` next to it.
///
/// `project` may be an existing project. In that case the unpack replaces
/// only `base/` and leaves `mod/` alone. It commits only once every file is
/// written, so a failure part way through leaves no half-made project.
///
/// Reads the disc once, front to back. Reports [`Step::Unpack`] across the
/// whole image, then [`Step::Save`], through `progress`. Returns whatever it
/// has to tell the user besides the project as [`Report`]s, in disc order.
///
/// # Errors
///
/// - [`Error::ForeignDirectory`] if `project` holds something else
/// - [`Error::Disc`] if the ISO can't be opened or read
/// - [`Error::Decode`] if a file on it isn't what its bytes claim
/// - [`Error::Io`] on any write
pub fn unpack(
    iso: &Path,
    project: &Path,
    progress: &Progress,
) -> Result<(Project, Vec<Report>), Error> {
    let reports = unpack::run(iso, project, progress)?;
    Ok((Project::discover(project)?, reports))
}

/// A finished unpack, by its canonical root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    /// Finds the project holding `dir` by walking upward from it, the way
    /// `git -C` starts its search from wherever it is pointed.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `dir` cannot be canonicalized
    /// - [`Error::NoProjectFound`] if nothing above `dir` is a project
    pub fn discover(dir: &Path) -> Result<Self> {
        Ok(Self {
            root: project::discover(dir)?,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The read-only unpack of the disc.
    #[must_use]
    pub fn base(&self) -> PathBuf {
        project::base(&self.root)
    }

    /// Every file the unpack recognised a leaf format in, by project path
    /// under `base/`, grouped by [`FileKind`]. Read from `.tpmt/formats`, so
    /// no file in `base/` is opened.
    ///
    /// Go through this, not file extensions, to find files of one kind. Names
    /// on the disc lie; the kinds here came from each file's magic.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `.tpmt/formats` is missing
    /// - [`Error::Parse`] if it is not what an unpack wrote
    pub fn formats(&self) -> Result<BTreeMap<FileKind, BTreeSet<String>>> {
        project::metadata::read_formats(&self.root)
    }

    /// Who the unpacked disc says it is, from `base/disc.toml`. Its game id
    /// and revision pick the version, through `tpmt_tables::Version::from_disc`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `base/disc.toml` is missing
    /// - [`Error::Parse`] if it is not what an unpack wrote
    pub fn boot(&self) -> Result<tpmt_disc::Boot> {
        project::metadata::read_boot(&self.base())
    }

    /// One project file's bytes, from `mod/overlay/` when it holds `path` and
    /// from `base/` otherwise. `path` is a project path, as
    /// [`formats`](Self::formats) lists.
    ///
    /// A `base/` copy isn't checked against its vanilla digest here, since
    /// that means reading every digest the unpack recorded.
    /// [`build`](Self::build) refuses one that drifted.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::MissingFile`] if neither layer holds a file at `path`
    /// - [`Error::Io`] if the file can't be read
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        leaf::read(&self.root, path)
    }

    /// Writes `data` to `path` under `mod/overlay/`, creating any missing
    /// directories. `base/` is never written.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::Io`] on the write
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        leaf::write(&self.root, path, data)
    }

    /// Hashes `mod/overlay/` against the vanilla digests taken at [`unpack`],
    /// and reports whatever doesn't match, sorted by path.
    ///
    /// An overlay file identical to vanilla is not a change. `base/` is not
    /// checked; a build refuses drift there when it reads the file.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] or [`Error::Parse`] if `.tpmt/` cannot be read back
    /// - [`Error::Io`] if a project file cannot be walked or read
    /// - [`Error::UnusablePath`] if a name in the project is not UTF-8
    pub fn status(&self) -> Result<Vec<Change>> {
        status::run(&self.root)
    }

    /// Re-encodes whatever `mod/overlay/` changed and hands it to `target`,
    /// which decides what to do with it: a tree of the changed disc files, a
    /// whole disc image, or a mod bundle.
    ///
    /// `output` stands in for the directory the target would otherwise own
    /// under `build/targets/`, and must be missing or empty.
    ///
    /// Reports [`Step::Rebuild`] through `progress`, and for an image
    /// [`Step::WriteImage`] as well.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`] if `output` is not empty
    /// - [`Error::Io`] or [`Error::Parse`] if the project's own files cannot
    ///   be read
    /// - [`Error::BaseModified`] if `base/` no longer matches the disc it came
    ///   from
    /// - [`Error::Encode`] if a rebuilt file does not fit its format
    /// - whatever else the target needs, which for an image is the source disc
    pub fn build(
        &self,
        target: Target,
        output: Option<&Path>,
        progress: &Progress,
    ) -> Result<Built> {
        build::run(&self.root, target, output, progress)
    }
}
