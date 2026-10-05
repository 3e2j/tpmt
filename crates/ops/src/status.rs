//! Listing what `mod/changes/` changes from vanilla.

use tpmt_project::{Change, Project, Store};

use crate::Result;

/// Every file in `mod/changes/` that differs from vanilla, sorted by path.
///
/// A file in `changes/` identical to vanilla is not a change, and a patch is
/// a change to the file it patches. `vanilla/` is not checked; a build
/// refuses drift there when it reads the file.
///
/// # Errors
///
/// - [`tpmt_project::Error`] if `.tpmt/` or a file in `changes/` can't be
///   read
pub fn status(project: &Project) -> Result<Vec<Change>> {
    let Store { digests, .. } = project.read_store()?;
    Ok(project.overlay().compare(&digests)?.changes)
}
