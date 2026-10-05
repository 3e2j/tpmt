//! What a frontend does with a project: unpack a disc into one, list what
//! changed, and build it.
//!
//! Each operation starts from `tpmt-project`, which says where the files
//! are, and hands what it read to `tpmt-packing`. Neither of those knows the
//! other. A frontend depends on this crate alone.

use std::path::Path;

mod build;
mod unpack;

pub use build::{Built, build};
pub use tpmt_packing::Target;
pub use tpmt_project::{Change, ChangeKind, Project, is_project};
pub use tpmt_report as report;
pub use unpack::unpack;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Project(#[from] tpmt_project::Error),

    #[error(transparent)]
    Packing(#[from] tpmt_packing::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The project holding `dir`, found by walking upward from it.
///
/// # Errors
///
/// - [`tpmt_project::Error::NoProjectFound`] if nothing above `dir` is one
/// - [`tpmt_project::Error::Io`] if `dir` cannot be canonicalized
pub fn discover(dir: &Path) -> Result<Project> {
    Ok(Project::discover(dir)?)
}

/// Every file in `mod/changes/` that differs from vanilla, sorted by path.
///
/// # Errors
///
/// - [`tpmt_project::Error`] if the project's records or files can't be read
pub fn status(project: &Project) -> Result<Vec<Change>> {
    Ok(project.diff()?)
}
