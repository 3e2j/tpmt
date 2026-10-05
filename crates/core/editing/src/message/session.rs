//! A message file open for editing, from what the project stores to what it
//! saves back.

use tpmt_binary::Format;
use tpmt_message::Bmg;
use tpmt_tables::Edition;

use super::patch::{self, BmgPatch, Names, PatchError};
use super::{BmgChanges, BmgEdit, EditError, EditableBmg, OpenError};
use crate::{History, Saved};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Open(#[from] OpenError),

    #[error(transparent)]
    Format(#[from] tpmt_message::Error),

    #[error(transparent)]
    Edit(#[from] EditError),

    #[error("the patch isn't UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("the patch doesn't read: {0}")]
    Read(#[from] toml::de::Error),

    #[error(transparent)]
    Patch(#[from] PatchError),

    #[error("the patch won't serialize: {0}")]
    Write(#[from] toml::ser::Error),
}

/// A message file open for editing. It changes only through
/// [`apply`](Self::apply), [`undo`](Self::undo) and [`redo`](Self::redo).
#[derive(Debug)]
pub struct BmgSession {
    file: EditableBmg,
    history: History<BmgEdit>,
    /// What the patch calls each new item, kept across saves so a name
    /// doesn't change under the modder.
    names: Names,
    /// The vanilla file a save diffs against, decoded once at open. `None`
    /// for a file stored whole, which saves whole.
    vanilla: Option<Bmg>,
    changes: BmgChanges,
}

impl BmgSession {
    /// The modder's own file, which saves whole.
    pub(crate) fn whole(bytes: &[u8], edition: Edition) -> Result<Self, SessionError> {
        Ok(Self::new(
            EditableBmg::open(bytes, edition)?,
            Names::default(),
            None,
        ))
    }

    /// A vanilla file with `patch` put over it, which saves as a patch.
    pub(crate) fn patched(
        vanilla: &[u8],
        patch: Option<&[u8]>,
        edition: Edition,
    ) -> Result<Self, SessionError> {
        let Some(patch) = patch else {
            let file = EditableBmg::open(vanilla, edition)?;
            let vanilla = file.bmg().clone();
            return Ok(Self::new(file, Names::default(), Some(vanilla)));
        };
        let vanilla = Bmg::decode(vanilla)?;
        let (bmg, names) = patch::apply(&vanilla, &read(patch)?, edition)?;
        Ok(Self::new(
            EditableBmg::new(bmg, edition),
            names,
            Some(vanilla),
        ))
    }

    fn new(file: EditableBmg, names: Names, vanilla: Option<Bmg>) -> Self {
        let changes = BmgChanges::new(vanilla.as_ref(), file.bmg());
        Self {
            file,
            history: History::default(),
            names,
            vanilla,
            changes,
        }
    }

    #[must_use]
    pub const fn file(&self) -> &EditableBmg {
        &self.file
    }

    /// How each record compares with vanilla, as of the last edit.
    #[must_use]
    pub const fn changes(&self) -> &BmgChanges {
        &self.changes
    }

    /// Performs `edit` and records how to undo it.
    ///
    /// # Errors
    ///
    /// When the file refuses the edit. Nothing changes then.
    pub fn apply(&mut self, edit: impl Into<BmgEdit>) -> Result<(), EditError> {
        self.history.apply(&mut self.file, edit.into())?;
        self.compare();
        Ok(())
    }

    /// Reverses the last edit. `Ok(false)` when there is nothing to undo.
    ///
    /// # Errors
    ///
    /// As [`History::undo`].
    pub fn undo(&mut self) -> Result<bool, EditError> {
        let undone = self.history.undo(&mut self.file)?;
        if undone {
            self.compare();
        }
        Ok(undone)
    }

    /// Performs the last undone edit again. `Ok(false)` when there is
    /// nothing to redo.
    ///
    /// # Errors
    ///
    /// As [`History::redo`].
    pub fn redo(&mut self) -> Result<bool, EditError> {
        let redone = self.history.redo(&mut self.file)?;
        if redone {
            self.compare();
        }
        Ok(redone)
    }

    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    #[must_use]
    pub const fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// The file as the project should store it: whole where it has no
    /// vanilla copy, and as a patch against that copy where it does.
    pub(crate) fn save(&mut self) -> Result<Saved, SessionError> {
        // Encoding checks the text a patch would otherwise carry unchecked.
        let bytes = self.file.save()?;
        let Some(vanilla) = &self.vanilla else {
            return Ok(Saved::Whole(bytes));
        };
        let edits = patch::diff(
            vanilla,
            self.file.bmg(),
            self.file.tables().edition,
            &mut self.names,
        );
        if edits.is_empty() {
            return Ok(Saved::Vanilla);
        }
        Ok(Saved::Patch(edits.to_toml()?))
    }

    fn compare(&mut self) {
        self.changes = BmgChanges::new(self.vanilla.as_ref(), self.file.bmg());
    }
}

/// The vanilla file `vanilla` with `patch` put over it, for a build.
pub fn apply(vanilla: &[u8], patch: &[u8], edition: Edition) -> Result<Vec<u8>, SessionError> {
    let vanilla = Bmg::decode(vanilla)?;
    let (bmg, _) = patch::apply(&vanilla, &read(patch)?, edition)?;
    Ok(bmg.encode()?)
}

fn read(patch: &[u8]) -> Result<BmgPatch, SessionError> {
    Ok(BmgPatch::from_toml(std::str::from_utf8(patch)?)?)
}
