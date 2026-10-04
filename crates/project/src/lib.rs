//! A project folder: where everything in it lives, what tpmt records in it,
//! and its files in and out. A front end goes through a [`Project`], found
//! with [`Project::discover`].
//!
//! Nothing here touches packaging. Making a project from a disc and building
//! it back is `tpmt-pipeline`'s job.
//!
//! A caller edits a leaf through three calls, none of which decode it.
//! [`Project::formats`] lists every leaf by kind. [`Project::read`] returns
//! one leaf's bytes, the `mod/overlay/` copy when there is one and the `base/`
//! copy otherwise. [`Project::write`] puts new bytes in `mod/overlay/`. The
//! caller hands the bytes to `tpmt-documents`, which decodes them.
//!
//! [`layout`], [`metadata`], [`fs`] and [`diff`] are public for the pipeline,
//! which writes `base/` and `.tpmt/` and reads them back. A front end has no
//! reason to reach past [`Project`].
//!
//! Project layout: see [`layout`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub mod diff;
pub mod fs;
pub mod layout;
mod leaf;
pub mod metadata;

pub use layout::is_project;
pub use tpmt_binary::FileKind;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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

    #[error("nothing in `base/` or `mod/overlay/` holds `{0}`")]
    MissingFile(String),
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
            root: layout::discover(dir)?,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The read-only unpack of the disc.
    #[must_use]
    pub fn base(&self) -> PathBuf {
        layout::base(&self.root)
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
        metadata::read_formats(&self.root)
    }

    /// Who the unpacked disc says it is, from `base/disc.toml`. Its game id
    /// and revision pick the version, through `tpmt_tables::Version::from_disc`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `base/disc.toml` is missing
    /// - [`Error::Parse`] if it is not what an unpack wrote
    pub fn boot(&self) -> Result<tpmt_disc::Boot> {
        metadata::read_boot(&self.base())
    }

    /// One project file's bytes, from `mod/overlay/` when it holds `path` and
    /// from `base/` otherwise. `path` is a project path, as
    /// [`formats`](Self::formats) lists.
    ///
    /// A `base/` copy isn't checked against its vanilla digest here, since
    /// that means reading every digest the unpack recorded. A build refuses
    /// one that drifted.
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

    /// Hashes `mod/overlay/` against the vanilla digests taken at unpack,
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
    pub fn diff(&self) -> Result<Vec<Change>> {
        diff::run(&self.root)
    }
}
