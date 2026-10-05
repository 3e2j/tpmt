//! Editing a format's decoded files, with undo, and saving the edits as a
//! patch against vanilla.
//!
//! This crate does three things:
//! 1. Provide a interface for editing formats (in packing and project)
//! 2. Associate game data tables with the formats
//! 3. Provide a [`Session`] that opens a file, edits it with history, and saves
//!    it with a patch
//!
//! An edit the format can't hold is refused.
//! Going against the game's tables is only warned.
//!
//! Performing an edit returns the edit that undoes it, so [`History`] keeps
//! two stacks of edits instead of snapshots.

pub mod message;
mod session;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub use session::{Error, Saved, Session, Source, apply};
pub use tpmt_tables::{Edition, Version};

/// A decoded file that changes only through edits.
pub trait Editable {
    type Edit;
    type Error;

    /// Applies `edit` and returns the edit that undoes it. On error the
    /// file is unchanged.
    ///
    /// # Errors
    ///
    /// When the edit doesn't fit the file, such as one naming something
    /// the file doesn't hold.
    fn perform(&mut self, edit: Self::Edit) -> Result<Self::Edit, Self::Error>;
}

/// How a record of an edited file compares with its vanilla copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unchanged,
    Changed,
    Added,
    Removed,
}

/// Each record of `vanilla` and `edited` with its [`Status`], matched by `key`.
///
/// Vanilla's records come first, then the added ones.
///
/// An edit must not change a record's key, or the record shows as removed and
/// a new one as added.
pub fn compare<R: PartialEq, K: Copy + Eq + Hash>(
    vanilla: &[R],
    edited: &[R],
    key: impl Fn(&R) -> K,
) -> Vec<(K, Status)> {
    // Where two records share a key, only the first counts.
    let mut by_key = HashMap::with_capacity(edited.len());
    for record in edited {
        by_key.entry(key(record)).or_insert(record);
    }

    let mut seen = HashSet::with_capacity(vanilla.len());
    let mut changes = Vec::with_capacity(vanilla.len().max(edited.len()));
    for record in vanilla {
        let key = key(record);
        if !seen.insert(key) {
            continue;
        }
        let status = match by_key.get(&key) {
            None => Status::Removed,
            Some(edited) if *edited == record => Status::Unchanged,
            Some(_) => Status::Changed,
        };
        changes.push((key, status));
    }

    for record in edited {
        let key = key(record);
        if seen.insert(key) {
            changes.push((key, Status::Added));
        }
    }
    changes
}

/// Undo and redo for one file, as stacks of the edits that reverse each
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
    /// Performs `edit` on `file` and records how to undo it. Clears the
    /// redo stack, since what it held branched off before this edit.
    ///
    /// # Errors
    ///
    /// When the file refuses the edit. Nothing is recorded then.
    pub fn apply<D: Editable<Edit = E>>(&mut self, file: &mut D, edit: E) -> Result<(), D::Error> {
        let inverse = file.perform(edit)?;
        self.undo.push(inverse);
        self.redo.clear();
        Ok(())
    }

    /// Reverses the last edit. `Ok(false)` when there is nothing to undo.
    ///
    /// # Errors
    ///
    /// When the file refuses the inverse, which means its `perform`
    /// handed back an inverse it can't apply. The step is dropped then.
    pub fn undo<D: Editable<Edit = E>>(&mut self, file: &mut D) -> Result<bool, D::Error> {
        let Some(edit) = self.undo.pop() else {
            return Ok(false);
        };
        self.redo.push(file.perform(edit)?);
        Ok(true)
    }

    /// Performs the last undone edit again. `Ok(false)` when there is nothing
    /// to redo.
    ///
    /// # Errors
    ///
    /// As [`undo`](Self::undo).
    pub fn redo<D: Editable<Edit = E>>(&mut self, file: &mut D) -> Result<bool, D::Error> {
        let Some(edit) = self.redo.pop() else {
            return Ok(false);
        };
        self.undo.push(file.perform(edit)?);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_match_by_key_not_position() {
        let vanilla = [(1, 'a'), (2, 'b'), (3, 'c')];
        let edited = [(4, 'd'), (3, 'c'), (1, 'z')];
        assert_eq!(
            compare(&vanilla, &edited, |record| record.0),
            [
                (1, Status::Changed),
                (2, Status::Removed),
                (3, Status::Unchanged),
                (4, Status::Added),
            ]
        );
    }
}
