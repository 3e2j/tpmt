//! A file open for editing whatever its format, so a caller that only moves
//! bytes between disk and here never names one.

use tpmt_binary::FileKind;
use tpmt_tables::{Edition, Version};

use crate::message::session;
use crate::message::{BmgSession, SessionError};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tpmt has no editor for this kind of file")]
    NotEditable,

    #[error("a patch exists, but tpmt has no patch logic for this kind of file")]
    Unpatchable,

    #[error(transparent)]
    Bmg(#[from] SessionError),
}

/// What the project holds for a file. The same two shapes `tpmt-project`
/// reads out of `mod/changes/`, borrowed, so neither crate depends on the
/// other.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// The modder's own file, with no vanilla copy.
    Whole(&'a [u8]),
    /// A vanilla file, with the patch over it if there is one.
    Vanilla {
        vanilla: &'a [u8],
        patch: Option<&'a [u8]>,
    },
}

/// What the project should store for a file after a save, one variant per
/// way `tpmt-project` writes to `mod/changes/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    Whole(Vec<u8>),
    Patch(String),
    /// The file matches vanilla, so any stored patch goes.
    Vanilla,
}

/// A file open for editing. Viewing and editing it means matching on its
/// format; undo, redo and saving don't.
#[derive(Debug)]
pub enum Session {
    Bmg(BmgSession),
}

impl Session {
    /// Opens `source` as whichever format its bytes are.
    ///
    /// # Errors
    ///
    /// - [`Error::NotEditable`] for a kind with no editor
    /// - the format's own error when the file or its patch doesn't read
    pub fn open(source: Source<'_>, version: Version) -> Result<Self, Error> {
        let bytes = match source {
            Source::Whole(bytes) | Source::Vanilla { vanilla: bytes, .. } => bytes,
        };
        match FileKind::identify(bytes) {
            Some(FileKind::Mesg) => {
                let edition = message_edition(version);
                let session = match source {
                    Source::Whole(bytes) => BmgSession::whole(bytes, edition)?,
                    Source::Vanilla { vanilla, patch } => {
                        BmgSession::patched(vanilla, patch, edition)?
                    }
                };
                Ok(Self::Bmg(session))
            }
            _ => Err(Error::NotEditable),
        }
    }

    /// What the project should now store for the file.
    ///
    /// # Errors
    ///
    /// When the file won't encode, or its patch won't serialize.
    pub fn save(&mut self) -> Result<Saved, Error> {
        match self {
            Self::Bmg(session) => Ok(session.save()?),
        }
    }

    /// As [`BmgSession::undo`].
    ///
    /// # Errors
    ///
    /// When the file refuses the inverse edit.
    pub fn undo(&mut self) -> Result<bool, Error> {
        match self {
            Self::Bmg(session) => Ok(session.undo().map_err(SessionError::from)?),
        }
    }

    /// As [`BmgSession::redo`].
    ///
    /// # Errors
    ///
    /// When the file refuses the edit.
    pub fn redo(&mut self) -> Result<bool, Error> {
        match self {
            Self::Bmg(session) => Ok(session.redo().map_err(SessionError::from)?),
        }
    }

    #[must_use]
    pub const fn can_undo(&self) -> bool {
        match self {
            Self::Bmg(session) => session.can_undo(),
        }
    }

    #[must_use]
    pub const fn can_redo(&self) -> bool {
        match self {
            Self::Bmg(session) => session.can_redo(),
        }
    }
}

/// The vanilla file `vanilla` with `patch` put over it, for a build.
///
/// # Errors
///
/// - [`Error::Unpatchable`] for a kind with no patch
/// - the format's own error when the file or its patch doesn't read or fit
pub fn apply(vanilla: &[u8], patch: &[u8], version: Version) -> Result<Vec<u8>, Error> {
    match FileKind::identify(vanilla) {
        Some(FileKind::Mesg) => Ok(session::apply(vanilla, patch, message_edition(version))?),
        _ => Err(Error::Unpatchable),
    }
}

/// Layouts and tags split by version alone, so the version's default
/// language serves every message folder.
const fn message_edition(version: Version) -> Edition {
    Edition::default_language(version)
}
