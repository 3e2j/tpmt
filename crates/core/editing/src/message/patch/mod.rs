//! A message file's edits as a patch against its vanilla copy, the form
//! `mod/changes/` keeps them in.
//!
//! A patch holds only what differs, so a mod can live in a git repo without
//! carrying the game's text. [`diff`](fn@diff) writes one from an edited
//! file, and [`apply`](fn@apply) puts it back over the vanilla one.
//!
//! The ids a decode hands out are never stored: they mean nothing outside one
//! run. Vanilla items go by what the vanilla file says about them instead,
//! and new ones by a name the patch gives them.
//!
//! Telling the two apart rests on one rule [`EditableBmg`](super::EditableBmg)
//! keeps: a vanilla item keeps the id its decode gave it, and a new one gets
//! an id past every vanilla one, even after a removal frees one.
//!
//! ```toml
//! [message.1234]          # a vanilla message, by its MID1 id
//! text = "Hello, {Player name}."
//! fields = { "Box kind" = 13 }
//!
//! [message."@12"]         # by vanilla position, where MID1 ids can't tell
//! attributes = "00110000" # whole, where no layout names the fields
//!
//! [node.87]               # a vanilla flow node, by vanilla position
//! next = "greet"
//!
//! [root]                  # flow ids, set or added
//! 300 = "node:4"
//!
//! [[new.message]]
//! name = "hello"
//! public_id = 9000
//! text = "Hi."
//!
//! [[new.node]]
//! name = "greet"
//! message = "hello"       # a vanilla message's key, or a new one's name
//! next = "node:88"        # "node:N" for a vanilla node, a name, or "end"
//!
//! [remove]
//! messages = ["1235"]
//! nodes = [90]
//! roots = [301]
//! ```
//!
//! A node's kind follows from its fields: `message` makes a text node,
//! `query`, `param` or `answers` a branch, and `event` or `params` an event.
//! New items go after the vanilla ones, in the order the patch lists them.
//!
//! [`diff`](fn@diff) is deterministic, so saving an unedited file writes
//! the same bytes, and a change put back by hand drops out of the patch.
//!
//! [`apply`](fn@apply) skips past a bad entry and goes on to the next, so a
//! patch with several mistakes names every one, each at its [`Entry`].

mod apply;
mod diff;
mod ids;
mod refs;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};
use tpmt_message::{MessageId, NodeId};
use tpmt_report::Diagnostic;

pub use apply::apply;
pub use diff::diff;
pub use refs::{MessageKey, MessageRef, NodeRef, Number};

use super::tables::text::TextError;

/// Changes to one message file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BmgPatch {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub message: BTreeMap<MessageKey, MessagePatch>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub node: BTreeMap<Number<u32>, NodePatch>,
    /// Every flow id that enters the graph somewhere new.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub root: BTreeMap<Number<u16>, NodeRef>,
    #[serde(skip_serializing_if = "New::is_empty")]
    pub new: New,
    #[serde(skip_serializing_if = "Remove::is_empty")]
    pub remove: Remove,
}

/// What changed in one vanilla message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MessagePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Attribute fields by their layout name. The id field is left to
    /// `public_id`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, u16>,
    /// The attributes whole, in hex, for a file no layout describes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

/// What changed in one vanilla node. A field the node's kind doesn't have
/// replaces the node with one of the kind it implies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<NodeRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<[u8; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<NodeRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct New {
    /// Gives a file with no flow graph one.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flow: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub message: Vec<NewMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub node: Vec<NewNode>,
}

/// A message the vanilla file doesn't have. Fields left out are 0.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NewMessage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<u16>,
    pub text: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

/// A node the vanilla file doesn't have. Its kind follows from its fields,
/// as for [`NodePatch`], and those it leaves out are 0 or `"end"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NewNode {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answers: Option<Vec<NodeRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<[u8; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<NodeRef>,
}

/// Vanilla items the edited file no longer has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Remove {
    /// Drops the flow graph whole.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flow: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<MessageKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<u16>,
}

/// The names [`apply`](fn@apply) gave new items, which [`diff`](fn@diff)
/// reuses so a save keeps them. Vanilla items have none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    pub messages: HashMap<MessageId, String>,
    pub nodes: HashMap<NodeId, String>,
}

/// A problem [`apply`](fn@apply) found, and the entry it's in.
pub type PatchDiagnostic = Diagnostic<PatchError, Entry>;

/// The part of a patch a [`PatchError`] is in, as the TOML writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Message(MessageKey),
    NewMessage(String),
    /// A vanilla node, by vanilla position.
    Node(u32),
    NewNode(String),
    Root(u16),
    Remove,
    /// `new.flow` or `remove.flow`.
    Flow,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(MessageKey::Id(id)) => write!(f, "[message.{id}]"),
            Self::Message(key) => write!(f, "[message.\"{key}\"]"),
            Self::NewMessage(name) => write!(f, "new message `{name}`"),
            Self::Node(at) => write!(f, "[node.{at}]"),
            Self::NewNode(name) => write!(f, "new node `{name}`"),
            Self::Root(id) => write!(f, "root {id}"),
            Self::Remove => f.write_str("[remove]"),
            Self::Flow => f.write_str("the flow graph"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("no vanilla message is `{0}`")]
    UnknownMessage(MessageKey),

    #[error("no vanilla node is `node:{0}`")]
    UnknownNode(u32),

    #[error("nothing new is named `{0}`")]
    UnknownName(String),

    #[error("the flow has no root {0}")]
    UnknownRoot(u16),

    #[error("two new items are named `{0}`")]
    DuplicateName(String),

    #[error(
        "`{0}` can't name a new item: a name can't be empty, be `end`, hold `:`, or start with a digit or `@`"
    )]
    BadName(String),

    #[error(transparent)]
    Text(TextError),

    #[error("the layout has no field `{0}`")]
    UnknownField(String),

    #[error("`{0}` is the message id; set `public_id` instead")]
    IdField(&'static str),

    #[error("{value} doesn't fit `{field}`")]
    ValueTooWide { field: &'static str, value: u16 },

    #[error("the attributes are {actual} bytes, and this file's records hold {expected}")]
    AttributeWidth { expected: usize, actual: usize },

    #[error("a file without MID1 has no public ids")]
    NoMid1,

    #[error("the node mixes fields of more than one kind")]
    MixedNode,

    #[error("the node needs a `message`, `query` or `event` to say what kind it is")]
    NoKind,

    #[error("the node has no `{0}`")]
    WrongField(&'static str),

    #[error("a root can't lead to `end`")]
    EndRoot,

    #[error("the file has no flow graph")]
    NoFlow,

    #[error("the file already has a flow graph")]
    HasFlow,

    #[error("the flow graph is removed, so nothing else in it can change")]
    RemovedFlow,

    #[error("it shows a message that was removed")]
    RemovedMessage,

    #[error("it leads to node `{0}`, which was removed")]
    RemovedNode(String),
}

impl BmgPatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// # Errors
    ///
    /// When TOML can't hold the patch, which a patch from
    /// [`diff`](fn@diff) never hits.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    /// # Errors
    ///
    /// When `text` isn't TOML, or isn't shaped like a patch.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

impl New {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Remove {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
