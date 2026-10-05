//! Making a project from a disc: packing walks it, and every file it hands
//! over lands in the project's `base/`.

use std::path::Path;

use tpmt_project::Project;
use tpmt_report::{Progress, Step};

use crate::Result;

/// Unpacks the disc at `iso` into a project at `root`, and returns it.
///
/// Writes `base/` and `.tpmt/`, and scaffolds an empty `mod/` next to them.
/// `root` may be an existing project. In that case the unpack replaces only
/// `base/` and leaves `mod/` alone. It commits only once every file is
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
    let base = project.new_base()?;

    let unpacked = tpmt_packing::unpack(iso, progress, |leaf| -> Result<_> {
        Ok(base.write(leaf.path, leaf.kind, leaf.bytes)?)
    })?;
    for dir in &unpacked.directories {
        base.create_dir(dir)?;
    }

    progress.begin(Step::Save, 0);
    base.finish(&unpacked.metadata, &unpacked.compressed)?;
    let boot = &unpacked.metadata.boot;
    project.write_store(iso, &boot.id, boot.revision, unpacked.stored)?;
    project.scaffold_mod()?;

    Ok(Project::discover(root)?)
}
