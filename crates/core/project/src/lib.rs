//! The project folder: where things live, what tpmt records, and every file
//! read or written inside it.
//!
//! This crate doesn't know what's inside a file. `tpmt-packing` takes the
//! disc apart and puts it back, `tpmt-editing` edits what comes out, and
//! `tpmt-ops` runs each operation through all three.
//!
//! A project is two directories, edited in place:
//!
//! ```text
//! vanilla/             read-only unpack of the ISO, decoded index for the UI to browse
//!   disc.toml          the preamble values a build cannot derive
//!   compression.toml   which loose files arrived wrapped, and in what
//!   sys/               apploader.img, main.dol
//!   files/             game content, archives as directories
//! mod/                 the mod project; the only directory a modder edits
//! changes/             files a build writes into the disc, at their disc paths:
//!                      patches (`<file>.toml`), replacements, and new files
//!   textures/          texture replacements, named by their Dolphin hash
//!   res/               content a runtime target loads beside the disc; a build
//!                      copies it as is and never writes it into the disc
//!     scripts/         Luau scripts, never parsed, copied into a build untouched
//!   mod.json           mod metadata (id, name, version, author, description, icon, banner)
//! build/
//!   targets/
//!     <target>/        what one build target produced, cleared and rewritten by it
//! ```
//!
//! Every unpack rewrites `vanilla/` whole, and writes `mod/` only if it is
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
// A modder who edits `vanilla/` directly should have those edits moved into
// `mod/changes/` automatically, and `vanilla/` restored. Hashing all of `vanilla/`
// on every build is too slow, so record each file's size and mtime at unpack
// and hash only the files whose size or mtime changed.
//
// Nothing reads `textures/` yet. Every target should: `dusk` copies it, and a
// disc build re-encodes each replacement into every file that holds a texture
// with that hash, found through an index of `vanilla/` taken at unpack.
//
// A native mod adds Dusklight's SDK template (`src/`, `CMakeLists.txt`,
// `cmake/`) to `mod/`. `dusk` should build it through the SDK and put the
// library under `lib/<platform>/` in the bundle.

use std::path::{Path, PathBuf};

use layout::{modding, store, vanilla};

mod build;
mod discover;
mod io;
mod layout;
mod overlay;
mod path;
mod unpack;

pub use discover::is_project;
pub use io::Staging;
pub use layout::store::{Digests, Payloads, Source, Store};
pub use layout::vanilla::Vanilla;
pub use overlay::{Change, ChangeKind, Comparison, Overlay, PATCH_SUFFIX, Stored, patch_path};
pub use tpmt_binary::{FileKind, Payload};
pub use unpack::{NewVanilla, Written};

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

    #[error("nothing in `vanilla/` or `mod/changes/` holds `{0}`")]
    MissingFile(String),

    #[error("`mod/changes/` holds `{0}` both whole and as a patch; keep one")]
    PatchConflict(String),

    /// A vanilla file is no longer what the unpack recorded, so a build off
    /// it would pack somebody's edit as though the disc had shipped it.
    #[error("`{0}` in `vanilla/` is not what was unpacked; re-unpack the disc, or put it back")]
    VanillaModified(String),
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

/// A project directory. One from [`discover`](Self::discover) is a finished
/// unpack, by its canonical root. One from [`claim`](Self::claim) is one
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

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The read-only unpack of the disc.
    #[must_use]
    pub fn vanilla(&self) -> PathBuf {
        self.root.join(vanilla::DIR)
    }

    /// `mod/changes/` over `vanilla/`, the way a build reads them.
    #[must_use]
    pub fn overlay(&self) -> Overlay {
        Overlay::new(
            self.vanilla(),
            modding::changes(&self.root.join(modding::DIR)),
        )
    }

    fn store(&self) -> PathBuf {
        self.root.join(store::DIR)
    }

    /// Every file the unpack recognised as a [`Payload`], by project path
    /// under `vanilla/`, grouped by payload. Read from `.tpmt/payloads`, so
    /// no file in `vanilla/` is opened.
    ///
    /// Go through this, not file extensions, to find files of one kind. Names
    /// on the disc lie; the kinds here came from each file's magic.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `.tpmt/payloads` is missing
    /// - [`Error::Parse`] if it is not what an unpack wrote, or names a kind
    ///   that isn't a payload
    pub fn payloads(&self) -> Result<Payloads> {
        store::read_payloads(&self.store())
    }

    /// `vanilla/disc.toml`, as whatever type wrote it.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `vanilla/disc.toml` is missing
    /// - [`Error::Parse`] if it is not a `D`
    pub fn read_disc<D: serde::de::DeserializeOwned>(&self) -> Result<D> {
        vanilla::read_disc(&self.vanilla())
    }

    /// Everything `vanilla/` says about itself, with `disc.toml` as whatever
    /// type wrote it.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `disc.toml` or `compression.toml` is missing
    /// - [`Error::Parse`] if either is not what an unpack wrote
    pub fn read_vanilla<D: serde::de::DeserializeOwned>(&self) -> Result<Vanilla<D>> {
        vanilla::read(&self.vanilla())
    }

    /// The source disc and vanilla digests from `.tpmt/`.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `source.toml` or `digests` is missing
    /// - [`Error::Parse`] if either is not what an unpack wrote
    pub fn read_store(&self) -> Result<Store> {
        store::read(&self.store())
    }

    /// What the project holds for one file: whole from `mod/changes/`, a
    /// patch there with its `vanilla/` copy, or the `vanilla/` copy alone. `path`
    /// is a project path, as [`payloads`](Self::payloads) lists.
    ///
    /// A `vanilla/` copy isn't checked against its digest here, since
    /// that means reading every digest the unpack recorded. A build refuses
    /// one that drifted.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::MissingFile`] if neither directory holds a file at `path`
    /// - [`Error::PatchConflict`] if `changes/` holds it whole and as a patch
    /// - [`Error::Io`] if a file can't be read
    pub fn read(&self, path: &str) -> Result<Stored> {
        self.overlay().read(path)
    }

    /// Writes `data` to `path` under `mod/changes/` whole, creating any
    /// missing directories. `vanilla/` is never written.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::PatchConflict`] if `changes/` holds a patch for `path`
    /// - [`Error::Io`] on the write
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        self.overlay().write(path, data)
    }

    /// Hashes `mod/changes/` against the vanilla `digests` file stored from an
    /// unpack, and reports whatever doesn't match, sorted by path.
    ///
    /// A file in `changes/` identical to vanilla is not a change, and a patch
    /// is a change to the file it patches. `vanilla/` is not checked; a build
    /// refuses drift there when it reads the file.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] or [`Error::Parse`] if `.tpmt/` cannot be read back
    /// - [`Error::Io`] if a project file cannot be walked or read
    /// - [`Error::UnusablePath`] if a name in the project is not UTF-8
    pub fn diff(&self) -> Result<Vec<Change>> {
        let store::Store { digests, .. } = self.read_store()?;
        Ok(self.overlay().compare(&digests)?.changes)
    }
}
