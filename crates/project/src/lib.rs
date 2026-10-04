//! Everything a frontend does with a project: unpack a disc into one, read
//! and write its files, and build it.
//!
//! This crate knows the project folder: where things live and what tpmt records.
//! It doesn't know what's inside a file. `tpmt-pipeline` takes the disc
//! apart and puts it back, and the format crates decode files.
//!
//! A project is two directories, edited in place:
//!
//! ```text
//! base/                read-only unpack of the ISO, decoded index for the UI to browse
//!   disc.toml          the preamble values a build cannot derive
//!   compression.toml   which loose files arrived wrapped, and in what
//!   sys/               apploader.img, main.dol
//!   files/             game content, archives as directories
//! mod/                 the mod project; the only directory a modder edits
//!   overlay/           whole-file / archive-member replacements, real paths
//!   res/               authored user-made content
//!     scripts/         Luau scripts, never parsed, copied into a build untouched
//!   mod.json           mod metadata (id, name, version, author, description, icon, banner)
//! build/
//!   targets/
//!     <target>/        what one build target produced, cleared and rewritten by it
//! ```
//!
//! Every unpack rewrites `base/` whole, and writes `mod/` only if it is
//! missing.
//!
//! Facts a decoded file can't carry, like a wrapper that came off it or
//! which memory an archive member loads into, live in a sidecar next to it
//! instead, one name per format:
//!
//! ```text
//! *.arc/.tpmt-arc.toml   what an unpacked archive is, minus its bytes
//! ```
//!
//! Everything generated about the project, rather than for it, lives in
//! `.tpmt/`. It goes in last, after everything else succeeded, so its
//! presence means an unpack finished, which is what [`is_project`] tests.
//!
// # TODO
// One disc per project FOR NOW.
//
// A modder may bring more than one region (GZ2E, GZ2P, GZ2J), in which case
// the first unpacked (by the modder) is the primary copy and each other region's
// files show only where their hash differs from the primary's file at the same path.
// That needs `digests` and `source.toml` keyed per region; both hold
// one disc today.
// Nothing here decides which regions an edit applies to yet.
//
// A modder who edits `base/` directly should have those edits moved into
// `mod/overlay/` automatically, and `base/` restored. Hashing all of `base/`
// on every build is too slow, so record each file's size and mtime at unpack
// and hash only the files whose size or mtime changed.

use std::path::{Path, PathBuf};

mod base;
mod build;
mod discover;
mod io;
mod layers;
mod mod_dir;
mod path;
mod store;
mod unpack;

pub use build::Built;
pub use discover::is_project;
pub use layers::{Change, ChangeKind, Comparison, Layer, Layers};
pub use store::Formats;
pub use tpmt_binary::FileKind;
pub use tpmt_pipeline::Target;

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

    /// A vanilla file is no longer what the unpack recorded, so a build off
    /// it would pack somebody's edit as though the disc had shipped it.
    #[error("`{0}` in `base/` is not what was unpacked; re-unpack the disc, or put it back")]
    BaseModified(String),

    #[error(transparent)]
    Pipeline(#[from] tpmt_pipeline::Error),
}

impl Error {
    /// Wraps an I/O failure at `path`, for `map_err`.
    pub fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Wraps a failure to parse the file at `path` after it read fine, for
    /// `map_err`.
    pub fn parse<E>(path: &Path) -> impl FnOnce(E) -> Self + '_
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        move |source| Self::Parse {
            path: path.to_path_buf(),
            source: source.into(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Build output, one directory per target under [`TARGETS_DIR`].
const BUILD_DIR: &str = "build";
const TARGETS_DIR: &str = "targets";

/// A project directory. One from [`discover`](Self::discover) is a finished
/// unpack, by its canonical root. Inside [`unpack`](Self::unpack), it is one
/// still being written.
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
            root: discover::discover(dir)?,
        })
    }

    /// Takes `root` for an unpack to write a project into, refusing it if it
    /// is not a project but already holds files.
    ///
    /// A project passes whatever else it holds (notes, fixtures, `.git`), since
    /// a re-unpack replaces only `base/`. An empty or missing directory passes,
    /// as does one holding only names this crate writes, from an unpack that
    /// failed part way.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`] if it holds anything else
    /// - [`Error::Io`] if it cannot be listed
    pub(crate) fn claim(root: &Path) -> Result<Self> {
        discover::refuse_foreign(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The read-only unpack of the disc.
    #[must_use]
    pub fn base(&self) -> PathBuf {
        self.root.join(base::DIR)
    }

    /// The modder's whole-file and archive-member replacements, addressed by
    /// the same project paths [`base`](Self::base) holds.
    #[must_use]
    pub fn overlay(&self) -> PathBuf {
        mod_dir::overlay(&self.root.join(mod_dir::DIR))
    }

    /// `mod/overlay/` over `base/`, the way a build reads them.
    #[must_use]
    pub fn layers(&self) -> Layers {
        Layers::new(self.base(), self.overlay())
    }

    /// Where one build target writes what it produced. A target owns its
    /// directory outright and clears it on every build, so two targets never
    /// read each other's leftovers.
    #[must_use]
    pub fn target_output(&self, target: &str) -> PathBuf {
        self.root.join(BUILD_DIR).join(TARGETS_DIR).join(target)
    }

    fn store(&self) -> PathBuf {
        self.root.join(store::DIR)
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
    /// - [`Error::Parse`] if it is not what an unpack wrote, or names a kind
    ///   this build doesn't know
    pub fn formats(&self) -> Result<Formats> {
        store::read_formats(&self.store())
    }

    /// Who the unpacked disc says it is, from `base/disc.toml`. Its game id
    /// and revision pick the version, through `tpmt_tables::Version::from_disc`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `base/disc.toml` is missing
    /// - [`Error::Parse`] if it is not what an unpack wrote
    pub fn boot(&self) -> Result<tpmt_disc::Boot> {
        base::read_boot(&self.base())
    }

    /// Everything `base/` says about itself.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `disc.toml` or `compression.toml` is missing
    /// - [`Error::Parse`] if either is not what an unpack wrote
    pub(crate) fn read_base(&self) -> Result<base::Base> {
        base::read(&self.base())
    }

    /// The source disc and vanilla digests from `.tpmt/`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `source.toml` or `digests` is missing
    /// - [`Error::Parse`] if either is not what an unpack wrote
    pub(crate) fn read_store(&self) -> Result<store::Store> {
        store::read(&self.store())
    }

    /// Writes `.tpmt/`, which is what makes this a project. The caller
    /// runs this last, once every other file is in place.
    ///
    /// Stores the ISO path canonicalized so later commands can read files off
    /// the original disc without asking the user where it is again.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `iso` cannot be canonicalized, or on any write
    /// - [`Error::UnusablePath`] if a path would not read back from a line of its own
    /// - [`Error::Serialize`] if a file will not serialize
    pub(crate) fn write_store(
        &self,
        iso: &Path,
        boot: &tpmt_disc::Boot,
        digests: &store::Digests,
        formats: &store::Formats,
    ) -> Result<()> {
        store::write(&self.store(), iso, boot, digests, formats)
    }

    /// Writes the `mod/` skeleton (`overlay/`, `res/scripts/`, a starter
    /// `mod.json`) alongside `base/`. Skips an existing `mod/`, so re-unpacking
    /// a project never clobbers a modder's edits.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if any of it cannot be written
    /// - [`Error::Serialize`] if `mod.json` will not serialize
    pub(crate) fn scaffold_mod(&self) -> Result<()> {
        let id = self
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mod");
        mod_dir::scaffold(&self.root.join(mod_dir::DIR), id)
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
        Ok(self.layers().read(path)?.1)
    }

    /// Writes `data` to `path` under `mod/overlay/`, creating any missing
    /// directories. `base/` is never written.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::Io`] on the write
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        self.layers().write(path, data)
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
        let store::Store { digests, .. } = self.read_store()?;
        Ok(self.layers().compare(&digests)?.changes)
    }
}
