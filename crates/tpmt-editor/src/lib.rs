//! Editable documents, one per format: where a decoded file meets the game's
//! tables.
//!
//! A document wraps a decoded file and changes it only through edits, which
//! are plain values. Performing an edit hands back the edit that undoes it, so
//! undo and redo are two stacks of edits ([`History`]) rather than snapshots.
//!
//! An edit fails only when the format can't hold the result. Anything the
//! format accepts goes through, even if the game wouldn't expect it, since a
//! mod can change what the game expects. Those edits warn instead, like a
//! branch node in a `jmessage` flow with more answers than its query returns.
//! An edit built on the game's tables, like setting a named record field, has
//! a raw counterpart that skips them.
//!
//! A document knows nothing about paths. It opens from bytes and saves to
//! bytes, and the pipeline decides where those go: `mod/overlay/`, never
//! `base/`.
//!
//! No UI framework here, so documents can be tested headless and reused by the
//! CLI or an export.

pub mod bmg;

/// A decoded file that changes only through edits.
pub trait Document {
    type Edit;
    type Error;

    /// Applies `edit` and returns the edit that undoes it. On error the
    /// document is unchanged.
    ///
    /// # Errors
    ///
    /// When the edit doesn't fit the document, such as one naming something
    /// the document doesn't hold.
    fn perform(&mut self, edit: Self::Edit) -> Result<Self::Edit, Self::Error>;
}

/// Undo and redo for one document, as stacks of the edits that reverse each
/// step.
#[derive(Debug)]
pub struct History<E> {
    undo: Vec<E>,
    redo: Vec<E>,
}

impl<E> Default for History<E> {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl<E> History<E> {
    /// Performs `edit` on `document` and records how to undo it. Clears the
    /// redo stack, since what it held branched off before this edit.
    ///
    /// # Errors
    ///
    /// When the document refuses the edit. Nothing is recorded then.
    pub fn apply<D: Document<Edit = E>>(
        &mut self,
        document: &mut D,
        edit: E,
    ) -> Result<(), D::Error> {
        let inverse = document.perform(edit)?;
        self.undo.push(inverse);
        self.redo.clear();
        Ok(())
    }

    /// Reverses the last edit. `Ok(false)` when there is nothing to undo.
    ///
    /// # Errors
    ///
    /// When the document refuses the inverse, which means its `perform`
    /// handed back an inverse it can't apply. The step is dropped then.
    pub fn undo<D: Document<Edit = E>>(&mut self, document: &mut D) -> Result<bool, D::Error> {
        let Some(edit) = self.undo.pop() else {
            return Ok(false);
        };
        self.redo.push(document.perform(edit)?);
        Ok(true)
    }

    /// Performs the last undone edit again. `Ok(false)` when there is nothing
    /// to redo.
    ///
    /// # Errors
    ///
    /// As [`undo`](Self::undo).
    pub fn redo<D: Document<Edit = E>>(&mut self, document: &mut D) -> Result<bool, D::Error> {
        let Some(edit) = self.redo.pop() else {
            return Ok(false);
        };
        self.undo.push(document.perform(edit)?);
        Ok(true)
    }

    #[must_use]
    pub const fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub const fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
