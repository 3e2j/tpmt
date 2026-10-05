//! Making a project from a disc: packing walks it, and every file it hands
//! over lands in the project's `vanilla/`.

use std::path::Path;

use tpmt_project::Project;
use tpmt_report::{Progress, Step};

use crate::Result;

/// Unpacks the disc at `iso` into a project at `root`, and returns it.
///
/// Writes `vanilla/` and `.tpmt/`, and scaffolds an empty `mod/` next to them.
/// `root` may be an existing project. In that case the unpack replaces only
/// `vanilla/` and leaves `mod/` alone. It commits only once every file is
/// written, so a failure part way through leaves no half-made project.
///
/// Reports [`Step::Unpack`] across the whole image, then [`Step::Save`],
/// through `progress`. Each file's reports go to `progress` as soon as that
/// file is unpacked.
///
/// # Errors
///
/// - [`tpmt_project::Error::ForeignDirectory`] if `root` holds something else
/// - [`tpmt_packing::Error`] if the ISO can't be read, or a file on it isn't
///   what its bytes claim
/// - [`tpmt_project::Error::Io`] on any write
pub fn unpack(iso: &Path, root: &Path, progress: &Progress) -> Result<Project> {
    let project = Project::claim(root)?;
    let vanilla = project.new_vanilla()?;

    let unpacked = tpmt_packing::unpack(iso, progress, |file| -> Result<_> {
        Ok(vanilla.write(file.path, file.kind, file.bytes)?)
    })?;
    for dir in &unpacked.directories {
        vanilla.create_dir(dir)?;
    }

    progress.begin(Step::Save, 0);
    vanilla.finish(&unpacked.metadata, &unpacked.compressed)?;
    let boot = &unpacked.metadata.boot;
    project.write_store(iso, &boot.id, boot.revision, unpacked.stored)?;
    project.scaffold_mod()?;

    Ok(Project::discover(root)?)
}
