//! Taking a disc apart into files, and putting changed files back into a
//! disc. Which folder those files live in is `tpmt-project`'s, and this
//! crate never names one: [`unpack`] hands each file to the caller to store,
//! and [`build`] reads them back through [`Files`].
//!
//! Unpack and build only handle containers: compression and archives.
//! A payload (BMG, ...) passes through both as raw bytes. Unpack sniffs
//! each file's magic to say what it is, but never decodes it.
//!
//! This crate owns what it takes to get from a disc to files and back (packaging):
//! the disc image, archives, compression, anything that wraps a payload.
//! A payload's own layout belongs to its format crate.

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

use std::path::{Path, PathBuf};

use tpmt_report::Progress;

mod build;
mod fs;
mod unpack;

pub use build::{Built, EncodeError, Files, Job, Source, Target};
pub use tpmt_disc::Metadata;
pub use unpack::{DecodeError, File, Unpacked, explode};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{}`: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),

    /// A format crate rejected one file, named by its project path so a
    /// member of a nested archive points at itself.
    #[error("`{path}`: {source}")]
    Decode { path: String, source: DecodeError },

    /// A format crate would not write one file back out, named the same way.
    #[error("`{path}`: {source}")]
    Encode { path: String, source: EncodeError },

    /// An archive's sidecar would not read back as one.
    #[error("could not read `{path}`: {source}")]
    Sidecar {
        path: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The image's layout lists a file that neither the source disc nor the
    /// rebuild supplies.
    #[error("nothing supplies `{0}` for the image")]
    Unsourced(String),

    #[error("the disc this project was unpacked from is no longer at `{}`", .0.display())]
    SourceMissing(PathBuf),

    /// The source disc now holds another version. Its unchanged files would
    /// not match the ones the unpack produced.
    #[error("`{}` holds {found}, but this project was unpacked from {unpacked}", .game_image.display())]
    SourceChanged {
        game_image: PathBuf,
        unpacked: String,
        found: String,
    },

    #[error("the {0} target is not implemented yet")]
    Unsupported(Target),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Walks the disc, peels off compression, opens archives, and calls `store`
/// with every file that comes out (see [`File`]).
///
/// Reads the disc once, front to back. Reports [`tpmt_report::Step::Unpack`]
/// across the whole image through `progress`.
///
/// `store` runs on several threads at once, one disc file per thread.
///
/// # Errors
///
/// - [`Error::Disc`] if the game image can't be opened or read
/// - [`Error::Decode`] if a file on it isn't what its bytes claim
/// - whatever `store` returns
pub fn unpack<T, E>(
    game_image: &Path,
    progress: &Progress,
    store: impl Fn(File<'_>) -> Result<T, E> + Sync,
) -> Result<Unpacked<T>, E>
where
    T: Send,
    E: From<Error> + Send,
{
    unpack::run(game_image, progress, store)
}

/// Rebuilds every disc file [`Job::changes`] touches and hands them to
/// `target`, which writes what it makes into `out`: a tree of the changed
/// disc files, a whole disc image, or a mod bundle.
///
/// Reports [`tpmt_report::Step::Rebuild`] through the job's progress, and for
/// an image [`tpmt_report::Step::WriteImage`] as well.
///
/// # Errors
///
/// - [`Error::Encode`] if a rebuilt file does not fit its format
/// - [`Error::Sidecar`] if an archive's sidecar won't read
/// - whatever reading a file through [`Job::files`] returns
/// - whatever else the target needs, which for an image is the source disc
pub fn build<E>(target: Target, job: &Job<'_, E>, out: &Path) -> Result<Built, E>
where
    E: From<Error> + Send,
{
    build::run(target, job, out)
}
