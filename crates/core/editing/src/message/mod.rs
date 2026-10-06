//! BMG message files, in four layers, each using only those above it:
//!
//! - [`tables`]: what the game's tables say about a file, its record fields
//!   and tag names
//! - [`edit`]: changing a file through edits, with undo
//! - [`check`]: what the tables find odd in a file the edits allow
//! - [`patch`]: a file's edits stored as the difference from vanilla
//!
//! [`BmgSession`] joins the last three for a file open in a frontend.

pub mod check;
pub mod edit;
pub mod patch;
pub(crate) mod session;
pub mod tables;

pub use check::{At, BmgDiagnostic, Issue, Item, Property};
pub use edit::{
    BmgChanges, BmgEdit, EditError, EditableBmg, FlowEdit, ListEdit, MessageChange, MessageEdit,
    NodeChange, NodeEdit, OpenError,
};
pub use patch::{Entry, PatchDiagnostic, PatchError};
pub use session::{BmgSession, SessionError};
pub use tables::text::{TextDiagnostic, TextError};
