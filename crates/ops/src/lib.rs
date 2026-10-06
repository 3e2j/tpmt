//! What a frontend does with a project, one module per operation:
//!
//! ```text
//! unpack   a disc into a new project
//! status   what mod/changes/ changes from vanilla
//! check    what's wrong or odd in mod/changes/
//! build    the changes, for one target
//! edit     open a file, and save it back to mod/changes/
//! ```
//!
//! On `Ok`, what a call found wrong or odd is on its result, such as
//! [`Built::reports`]. On `Err`, [`Error::reports`] lists the problems that
//! stopped it, if any, and the error itself says why it stopped.
//!
//! `tpmt-project` finds the files, and `tpmt-packing` and `tpmt-editing` work
//! on them. None of the three depends on another, so this crate joins them.

use std::path::Path;

use tpmt_editing::Version;
use tpmt_packing::Metadata;
use tpmt_report::{Counts, Report};

mod build;
mod check;
mod edit;
#[cfg(test)]
mod fixture;
mod status;
mod unpack;

pub use build::{Built, build};
pub use check::check;
pub use edit::{Open, open, save};
pub use status::status;
pub use tpmt_editing as editing;
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

    #[error("tpmt has no tables for the unpacked disc's version, so it can't read patches")]
    UnknownVersion,

    /// A file or its patch wouldn't decode, apply, or encode.
    #[error("`{path}`: {source}")]
    File {
        path: String,
        source: tpmt_editing::Error,
    },

    /// The check before a build found errors. Holds every report, warnings
    /// too.
    #[error("the build stopped at {}", errors(.0))]
    Rejected(Vec<Report>),
}

impl Error {
    /// Each problem behind this error, for one that stands for several.
    /// Empty when the error says it all.
    #[must_use]
    pub fn reports(&self) -> Vec<Report> {
        match self {
            Self::File { path, source } => source.reports(path),
            Self::Rejected(reports) => reports.clone(),
            _ => Vec::new(),
        }
    }

    /// Wraps a failure to open, patch or save the file at `path`, for
    /// `map_err`.
    fn file(path: &str) -> impl FnOnce(tpmt_editing::Error) -> Self + '_ {
        move |source| Self::File {
            path: path.to_string(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// "1 error", "3 errors".
fn errors(reports: &[Report]) -> String {
    match Counts::of(reports).errors {
        1 => "1 error".to_string(),
        count => format!("{count} errors"),
    }
}

/// The version `disc` is, or `None` when tpmt has no tables for it.
fn version(disc: &Metadata) -> Option<Version> {
    Version::from_disc(&disc.boot.id, disc.boot.revision)
}

/// The project holding `dir`, found by walking upward from it.
///
/// # Errors
///
/// - [`tpmt_project::Error::NoProjectFound`] if nothing above `dir` is one
/// - [`tpmt_project::Error::Io`] if `dir` cannot be canonicalized
pub fn discover(dir: &Path) -> Result<Project> {
    Ok(Project::discover(dir)?)
}
