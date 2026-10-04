//! Unpacking a disc into a project folder, and building it back. The
//! project itself, and every file in it, is `tpmt-project`'s.
//!
//! This crate works on the whole disc at once, so it only touches packaging.
//! Working with a single unpacked file (a payload), is `tpmt-project`'s job.
//!
//! Unpack and build only handle containers: compression and archives.
//! A leaf format (BMG, ...) passes through both as raw bytes. Unpack sniffs
//! each file's magic to record its kind in `.tpmt/formats`, but never
//! decodes it.
//!
//! This crate owns what it takes to get from a disc to a project and back:
//! the disc image, archives, compression, and anything that spans files, like
//! cross-references. A leaf's own layout belongs to its format crate.

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

use tpmt_project::Project;
use tpmt_project::io::{Staging, fs};
use tpmt_project::store::{Digests, Formats, digest};
use tpmt_report::{Progress, Step};

mod build;
mod unpack;

pub use build::{Built, EncodeError, Target};
pub use unpack::explode::{DecodeError, Layer, file as explode};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Project(#[from] tpmt_project::Error),

    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),

    /// A format crate rejected one file, named by its project path so a
    /// member of a nested archive points at itself.
    #[error("`{path}`: {source}")]
    Decode { path: String, source: DecodeError },

    /// A format crate would not write one file back out, named the same way.
    #[error("`{path}`: {source}")]
    Encode { path: String, source: EncodeError },

    /// A vanilla file is no longer what the unpack recorded, so a build off
    /// it would pack somebody's edit as though the disc had shipped it.
    #[error("`{0}` in `base/` is not what was unpacked; re-unpack the disc, or put it back")]
    BaseModified(String),

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
/// whole image, then [`Step::Save`], through `progress`. Each file's reports
/// go to `progress` as soon as that file is unpacked.
///
/// # Errors
///
/// - [`tpmt_project::Error::ForeignDirectory`] if `project` holds something
///   else
/// - [`Error::Disc`] if the ISO can't be opened or read
/// - [`Error::Decode`] if a file on it isn't what its bytes claim
/// - [`tpmt_project::Error::Io`] on any write
pub fn unpack(iso: &Path, project: &Path, progress: &Progress) -> Result<Project> {
    // TODO: moves into `tpmt-project` once build stops depending on it.
    let claimed = Project::claim(project)?;
    let staging = Staging::begin(&claimed.base())?;
    let base = staging.dir();
    let unpacked = unpack::run(iso, progress, |leaf| -> Result<_> {
        fs::write(&base.join(leaf.path), leaf.bytes)?;
        Ok((leaf.path.to_string(), digest(leaf.bytes), leaf.kind))
    })?;
    for dir in &unpacked.directories {
        fs::create_dir_all(&base.join(dir))?;
    }

    progress.begin(Step::Save, 0);
    tpmt_project::base::write(base, &unpacked.metadata, unpacked.yaz0_compressed)?;
    staging.promote()?;

    let mut digests = Digests::new();
    let mut formats = Formats::new();
    for (path, digest, kind) in unpacked.stored {
        if let Some(kind) = kind {
            formats.entry(kind).or_default().insert(path.clone());
        }
        digests.insert(path, digest);
    }
    claimed.write_store(iso, &unpacked.metadata.boot, &digests, &formats)?;
    claimed.scaffold_mod()?;

    Project::discover(project).map_err(Error::from)
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
/// - [`tpmt_project::Error::ForeignDirectory`] if `output` is not empty
/// - [`Error::Project`] if the project's own files cannot be read
/// - [`Error::BaseModified`] if `base/` no longer matches the disc it came
///   from
/// - [`Error::Encode`] if a rebuilt file does not fit its format
/// - whatever else the target needs, which for an image is the source disc
pub fn build(
    project: &Project,
    target: Target,
    output: Option<&Path>,
    progress: &Progress,
) -> Result<Built> {
    build::run(project, target, output, progress)
}
