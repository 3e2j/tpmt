//! Opening a project file for editing and saving it back to `mod/changes/`.
//!
//! What a file opens as and how it saves is up to `tpmt-editing`. This only
//! moves what the project stores in and out of a [`Session`].

use tpmt_editing::{Saved, Session, Source};
use tpmt_packing::Metadata;
use tpmt_project::{Project, Stored};

use crate::{Error, Result, version};

/// A project file open for editing. Nothing reaches the project until
/// [`save`].
#[derive(Debug)]
pub struct Open {
    path: String,
    session: Session,
}

impl Open {
    /// The project path this file saves to.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn session(&self) -> &Session {
        &self.session
    }

    pub const fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
}

/// The file at `path` as `mod/changes/` leaves it: the vanilla file with its
/// patch put over it, the modder's own file whole, or the vanilla file
/// untouched.
///
/// # Errors
///
/// - [`tpmt_project::Error`] if `path` is unusable, missing, stored whole
///   and as a patch, or can't be read
/// - [`tpmt_project::Error`] if `vanilla/disc.toml` won't read
/// - [`Error::UnknownVersion`] if tpmt has no tables for the disc
/// - [`Error::File`] if the file has no editor, or it or its patch doesn't
///   read
pub fn open(project: &Project, path: &str) -> Result<Open> {
    let version = version(&project.read_disc::<Metadata>()?).ok_or(Error::UnknownVersion)?;
    let stored = project.overlay().read(path)?;
    let source = match &stored {
        Stored::Whole(bytes) => Source::Whole(bytes),
        Stored::Vanilla { vanilla, patch } => Source::Vanilla {
            vanilla,
            patch: patch.as_deref(),
        },
    };
    let session = Session::open(source, version).map_err(Error::file(path))?;
    Ok(Open {
        path: path.to_string(),
        session,
    })
}

/// Saves `open` to `mod/changes/` in whatever form its session asks for. A
/// file back to vanilla has its patch removed.
///
/// # Errors
///
/// - [`Error::File`] if the file won't encode, or its patch won't serialize
/// - [`tpmt_project::Error`] on the write
pub fn save(project: &Project, open: &mut Open) -> Result<()> {
    let path = open.path.as_str();
    let overlay = project.overlay();
    match open.session.save().map_err(Error::file(path))? {
        Saved::Whole(bytes) => overlay.write(path, &bytes)?,
        Saved::Patch(text) => overlay.write_patch(path, text.as_bytes())?,
        Saved::Vanilla => overlay.remove_patch(path)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tpmt_editing::message::{BmgSession, MessageChange, MessageEdit};
    use tpmt_message::{MessageId, TextSegment};
    use tpmt_project::patch_path;

    use super::*;
    use crate::fixture::{self, message_file};

    const PATH: &str = "files/res/Msgus/bmgres.arc/zel_00.bmg";

    fn set_text(text: &[u8]) -> MessageEdit {
        MessageEdit::Set {
            id: MessageId(0),
            change: MessageChange::Text(vec![TextSegment::Text(text.into())]),
        }
    }

    fn bmg(file: &mut Open) -> &mut BmgSession {
        let Session::Bmg(bmg) = file.session_mut();
        bmg
    }

    /// The file `mod/changes/` holds whole at `path`, if any.
    fn changes(project: &Project, path: &str) -> Option<Vec<u8>> {
        match project.overlay().read(path) {
            Ok(Stored::Whole(data)) => Some(data.into_vec()),
            _ => None,
        }
    }

    #[test]
    fn an_edit_saves_as_a_patch_and_opens_again() {
        let (_scratch, project) = fixture::project(&[(PATH, &message_file(b"Hello"))]);
        let mut file = open(&project, PATH).unwrap();
        bmg(&mut file).apply(set_text(b"Goodbye")).unwrap();
        save(&project, &mut file).unwrap();

        assert_eq!(
            changes(&project, &patch_path(PATH)).unwrap(),
            b"[message.\"@0\"]\ntext = \"Goodbye\"\n"
        );
        assert_eq!(changes(&project, PATH), None);
        assert_eq!(
            bmg(&mut open(&project, PATH).unwrap()).file(),
            bmg(&mut file).file()
        );
    }

    #[test]
    fn an_edit_put_back_removes_the_patch() {
        let (_scratch, project) = fixture::project(&[(PATH, &message_file(b"Hello"))]);
        let mut file = open(&project, PATH).unwrap();
        bmg(&mut file).apply(set_text(b"Goodbye")).unwrap();
        save(&project, &mut file).unwrap();
        file.session_mut().undo().unwrap();
        save(&project, &mut file).unwrap();
        assert_eq!(changes(&project, &patch_path(PATH)), None);
    }

    /// A file the disc doesn't have has nothing to patch against.
    #[test]
    fn a_file_the_modder_made_saves_whole() {
        let (_scratch, project) = fixture::project(&[]);
        project
            .overlay()
            .write(PATH, &message_file(b"Hello"))
            .unwrap();
        let mut file = open(&project, PATH).unwrap();
        bmg(&mut file).apply(set_text(b"Goodbye")).unwrap();
        save(&project, &mut file).unwrap();

        assert_eq!(changes(&project, PATH).unwrap(), message_file(b"Goodbye"));
        assert_eq!(changes(&project, &patch_path(PATH)), None);
    }
}
