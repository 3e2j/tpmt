//! BMG message files, in three layers, each using only those above it:
//!
//! - [`tables`]: what the game's tables say about a file, its record fields
//!   and tag names
//! - [`edit`]: changing a file through edits, with undo
//! - [`patch`]: a file's edits stored as the difference from vanilla
//!
//! [`BmgSession`] joins the last two for a file open in a frontend.

pub mod edit;
pub mod patch;
pub(crate) mod session;
pub mod tables;

pub use edit::{
    BmgChanges, BmgEdit, EditError, EditableBmg, FlowEdit, ListEdit, MessageChange, MessageEdit,
    NodeChange, NodeEdit, OpenError,
};
pub use session::{BmgSession, SessionError};
