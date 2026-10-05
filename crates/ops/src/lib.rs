//! What a frontend does with a project: unpack a disc into one, list what
//! changed, and build it.
//!
//! `tpmt-project` finds the files, and `tpmt-packing` and `tpmt-editing` work
//! on them. None of the three depends on another, so this crate joins them.

use std::path::Path;

use tpmt_editing::Version;
use tpmt_packing::Metadata;

mod build;
mod edit;
mod unpack;

pub use build::{Built, build};
pub use edit::{Open, open, save};
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
}

impl Error {
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

/// The version the unpacked disc is, or `None` when tpmt has no tables for
/// it.
///
/// # Errors
///
/// - [`tpmt_project::Error`] if `vanilla/disc.toml` won't read
fn version(project: &Project) -> Result<Option<Version>> {
    let disc = project.read_disc::<Metadata>()?;
    Ok(Version::from_disc(&disc.boot.id, disc.boot.revision))
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

/// Every file in `mod/changes/` that differs from vanilla, sorted by path.
///
/// # Errors
///
/// - [`tpmt_project::Error`] if the project's records or files can't be read
pub fn status(project: &Project) -> Result<Vec<Change>> {
    Ok(project.diff()?)
}

/// Unpacked projects for tests, built by hand.
#[cfg(test)]
mod fixture {
    use std::collections::BTreeMap;
    use std::path::Path;

    use tempfile::TempDir;
    use tpmt_binary::{FileKind, Format};
    use tpmt_disc::{Bi2, Boot, Metadata};
    use tpmt_message::{Bmg, Encoding, Message, MessageId, TextSegment};
    use tpmt_project::{Project, Written};
    use tpmt_tables::message::record;

    /// A `GZ2E` revision 0 disc.
    pub fn metadata() -> Metadata {
        Metadata {
            boot: Boot {
                id: "GZ2E".to_string(),
                maker: "01".to_string(),
                disc_number: 0,
                revision: 0,
                audio_streaming: 0,
                stream_buffer_size: 0,
                title: "test".to_string(),
            },
            bi2: Bi2 {
                simulated_memory_size: 0x0180_0000,
                debug_flag: 0,
                country: 1,
                unknown_1c: 1,
                unknown_20: 1,
                pad_spec: 0,
            },
        }
    }

    /// A message file of one message saying `text`.
    pub fn message_file(text: &[u8]) -> Vec<u8> {
        Bmg {
            encoding: Encoding::ShiftJis,
            record_len: record::LAYOUT.len,
            mid1: None,
            messages: vec![Message {
                public_id: 0,
                id: MessageId(0),
                attributes: Box::new([0; 16]),
                text: vec![TextSegment::Text(text.into())],
            }],
            flow: None,
            strings: None,
        }
        .encode()
        .unwrap()
    }

    /// A finished unpack of `metadata()` whose `vanilla/` holds `files`.
    pub fn project(files: &[(&str, &[u8])]) -> (TempDir, Project) {
        let scratch = tempfile::tempdir().unwrap();
        let project = Project::claim(scratch.path()).unwrap();
        let vanilla = project.new_vanilla().unwrap();
        let written: Vec<Written> = files
            .iter()
            .map(|(path, data)| vanilla.write(path, FileKind::identify(data), data).unwrap())
            .collect();
        vanilla.finish(&metadata(), &BTreeMap::new()).unwrap();
        finish(&project, scratch.path(), written);
        let project = Project::discover(scratch.path()).unwrap();
        (scratch, project)
    }

    /// Writes the store, which marks the unpack finished.
    pub fn finish(project: &Project, root: &Path, written: Vec<Written>) {
        // Nothing in these tests opens the disc; `write_store` only wants a
        // path it can canonicalize.
        let iso = root.join("source.iso");
        std::fs::write(&iso, b"").unwrap();
        project.write_store(&iso, "GZ2E", 0, written).unwrap();
    }
}
