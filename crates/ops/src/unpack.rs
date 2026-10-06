//! Making a project from a disc: packing walks it, and every file it hands
//! over lands in the project's `vanilla/`.

use std::path::Path;

use tpmt_project::{Project, Record};
use tpmt_report::{Progress, Step};

use crate::Result;

/// Unpacks the disc at `game_image` into a project at `root`, and returns it.
///
/// Writes `vanilla/` and `.tpmt/`, and scaffolds an empty `mod/` next to them.
/// `root` may be an existing project. In that case the unpack replaces only
/// `vanilla/` and leaves `mod/` alone. It commits only once every file is
/// written, so a failure part way through leaves no half-made project.
///
/// Reports [`Step::Unpack`] across the whole image, then [`Step::Save`],
/// through `progress`.
///
/// # Errors
///
/// - [`tpmt_project::Error::ForeignDirectory`] if `root` holds something else
/// - [`tpmt_packing::Error`] if the game image can't be read, or a file on it isn't
///   what its bytes claim
/// - [`tpmt_project::Error::Io`] on any write
pub fn unpack(game_image: &Path, root: &Path, progress: &Progress) -> Result<Project> {
    let unpacking = Project::unpack(root)?;
    // Begins Step::Unpack in here
    let unpacked = tpmt_packing::unpack(game_image, progress, |file| -> Result<_> {
        Ok(unpacking.write(file.path, file.kind, file.bytes)?)
    })?;

    progress.begin(Step::Save, 0);
    let boot = &unpacked.metadata.boot;
    let record = Record {
        game_image,
        id: &boot.id,
        revision: boot.revision,
        disc_metadata: &unpacked.metadata,
        compressed: &unpacked.compressed,
        directories: &unpacked.directories,
    };
    Ok(unpacking.finish(&record, unpacked.stored)?)
}
