//! Editing a BMG message file through edits that are plain values.
//!
//! Text is kept as `tpmt-message`'s segments, so nothing is re-parsed on save.
//!
//! Edits are checked for what would leave the graph pointing at nothing: a
//! removed message a text node still shows, a removed node an edge still
//! reaches. Malformed text is left to [`EditableBmg::save`], which refuses
//! it the same way the encoder does.
//!
//! A new message or node always gets an id past every one the file opened
//! with. [`patch`](super::patch) relies on that to tell vanilla items from
//! new ones.

use std::mem;

use tpmt_binary::Format;
use tpmt_message::{Bmg, Flow, Message, MessageId, Node, NodeId, Root, TextSegment};
use tpmt_tables::Edition;
use tpmt_tables::message::Field;

use super::tables::{Tables, field_bytes, read_field, write_field};
use crate::{Editable, Status, compare};

/// One change to a message file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BmgEdit {
    Message(MessageEdit),
    Flow(FlowEdit),
}

/// A change to the message list or to one message in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageEdit {
    Set {
        id: MessageId,
        change: MessageChange,
    },
    Insert {
        at: usize,
        message: Message,
    },
    Remove(MessageId),
}

// TODO: text changes only whole, so retagging one tag sends every segment and
// undo keeps the old list in full. A `Segment(ListEdit<TextSegment>)` change
// would edit one segment at a time, like `NodeChange::Answer` and
// `FlowEdit::Root` do for their lists.

/// A new value for one part of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageChange {
    Text(Vec<TextSegment>),
    /// One named field of the attributes.
    Field {
        field: Field,
        value: u16,
    },
    /// The attributes whole, for records [`Field`]s don't describe or bytes
    /// none of them cover.
    Attributes(Box<[u8]>),
    /// In a file with a MID1, also rewrites the copy of the id in the
    /// attributes.
    PublicId(u16),
}

/// A change to the flow graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowEdit {
    /// Gives a file with no flow graph an empty one.
    Create,
    /// Drops an empty flow graph, so the file has no flow sections.
    Delete,
    Node(NodeEdit),
    Root(ListEdit<Root>),
}

/// A change to the node list or to one node in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeEdit {
    Set { id: NodeId, change: NodeChange },
    Insert { at: usize, node: Node },
    Remove(NodeId),
}

/// A new value for one part of a node. Each fits only some node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeChange {
    /// The message a text node shows.
    Shown(MessageId),
    /// Where a text or event node carries on. `None` ends the conversation.
    Next(Option<NodeId>),
    Query {
        query: u16,
        param: u16,
    },
    Event {
        event: u8,
        params: [u8; 4],
    },
    /// Where a branch node's answers lead.
    Answer(ListEdit<Option<NodeId>>),
}

/// A change to a list addressed by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEdit<T> {
    Set { at: usize, value: T },
    Insert { at: usize, value: T },
    Remove { at: usize },
}

impl From<MessageEdit> for BmgEdit {
    fn from(edit: MessageEdit) -> Self {
        Self::Message(edit)
    }
}

impl From<FlowEdit> for BmgEdit {
    fn from(edit: FlowEdit) -> Self {
        Self::Flow(edit)
    }
}

impl From<NodeEdit> for BmgEdit {
    fn from(edit: NodeEdit) -> Self {
        Self::Flow(FlowEdit::Node(edit))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Decode(#[from] tpmt_message::Error),

    /// The edits keep the two equal, so a file where they differ can't be
    /// edited without losing one of them.
    #[error("message {} has id {record} in its record, and {public_id} in MID1", .message.0)]
    IdMismatch {
        message: MessageId,
        public_id: u16,
        record: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("no message has internal id {}", .0.0)]
    UnknownMessage(MessageId),

    #[error("no flow node has id {}", .0.0)]
    UnknownNode(NodeId),

    #[error("a message with internal id {} already exists", .0.0)]
    DuplicateMessage(MessageId),

    #[error("a flow node with id {} already exists", .0.0)]
    DuplicateNode(NodeId),

    #[error("a flow node still shows message {}", .0.0)]
    MessageInUse(MessageId),

    #[error("a flow edge or root still reaches node {}", .0.0)]
    NodeInUse(NodeId),

    #[error("flow node {} is not a {expected} node", .node.0)]
    WrongKind {
        node: NodeId,
        expected: &'static str,
    },

    #[error("the attributes are {actual} bytes, and this file's records hold {expected}")]
    AttributeWidth { expected: usize, actual: usize },

    #[error("the attributes don't hold a field at record offset {0:#x}")]
    FieldOutOfRange(usize),

    #[error("{value} doesn't fit a {len}-byte field")]
    ValueTooWide { value: u16, len: usize },

    #[error("in a file with a MID1 the id field is the public id, so set that instead")]
    IdField,

    #[error("position {0} is past the end")]
    OutOfRange(usize),

    #[error("the file has no flow graph")]
    NoFlow,

    #[error("the file already has a flow graph")]
    HasFlow,

    #[error("the flow graph still has nodes or roots")]
    FlowNotEmpty,
}

/// How each record of an edited message file compares with vanilla, as
/// [`compare`] orders them. Against no vanilla file, every record is added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BmgChanges {
    pub messages: Vec<(MessageId, Status)>,
    pub nodes: Vec<(NodeId, Status)>,
    /// By public id, since that's how a patch names a root.
    pub roots: Vec<(u16, Status)>,
}

impl BmgChanges {
    #[must_use]
    pub fn new(vanilla: Option<&Bmg>, edited: &Bmg) -> Self {
        let vanilla_flow = vanilla.and_then(|bmg| bmg.flow.as_ref());
        let edited_flow = edited.flow.as_ref();
        Self {
            messages: compare(
                vanilla.map_or(&[][..], |bmg| &bmg.messages),
                &edited.messages,
                |message| message.id,
            ),
            nodes: compare(
                vanilla_flow.map_or(&[][..], |flow| &flow.nodes),
                edited_flow.map_or(&[][..], |flow| &flow.nodes),
                Node::id,
            ),
            roots: compare(
                vanilla_flow.map_or(&[][..], |flow| &flow.roots),
                edited_flow.map_or(&[][..], |flow| &flow.roots),
                |root| root.public_id,
            ),
        }
    }
}

/// A message file open for editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditableBmg {
    bmg: Bmg,
    tables: Tables,
    /// The lowest ids a new message and a new node may take: past every one
    /// the file opened with, so an id freed by a removal is never handed out
    /// again.
    first_new: (MessageId, NodeId),
}

impl EditableBmg {
    /// # Errors
    ///
    /// When the bytes aren't a BMG, or are a broken one, or a message's
    /// record holds a different id than its MID1 entry.
    pub fn open(bytes: &[u8], edition: Edition) -> Result<Self, OpenError> {
        let file = Self::new(Bmg::decode(bytes)?, edition);
        if let Some(id) = file.tables.id {
            for message in &file.bmg.messages {
                if let Some(record) = read_field(&message.attributes, &id)
                    && record != message.public_id
                {
                    return Err(OpenError::IdMismatch {
                        message: message.id,
                        public_id: message.public_id,
                        record,
                    });
                }
            }
        }
        Ok(file)
    }

    /// An already decoded file, with no id check.
    #[must_use]
    pub fn new(bmg: Bmg, edition: Edition) -> Self {
        Self {
            tables: Tables::new(&bmg, edition),
            first_new: (next_message_id(&bmg), next_node_id(&bmg)),
            bmg,
        }
    }

    /// The file as bytes, for `tpmt-ops` to write into `changes/`.
    ///
    /// # Errors
    ///
    /// When the encoder refuses the file, such as a text run holding a tag
    /// opener. See [`Bmg::encode`].
    pub fn save(&self) -> Result<Vec<u8>, tpmt_message::Error> {
        self.bmg.encode()
    }

    #[must_use]
    pub const fn bmg(&self) -> &Bmg {
        &self.bmg
    }

    /// What the game's tables say about this file.
    #[must_use]
    pub const fn tables(&self) -> &Tables {
        &self.tables
    }

    #[must_use]
    pub fn message(&self, id: MessageId) -> Option<&Message> {
        self.bmg.messages.iter().find(|message| message.id == id)
    }

    /// An internal id no message holds or held, for [`MessageEdit::Insert`].
    #[must_use]
    pub fn unused_message_id(&self) -> MessageId {
        MessageId(next_message_id(&self.bmg).0.max(self.first_new.0.0))
    }

    /// A node id no node holds or held, for [`NodeEdit::Insert`].
    #[must_use]
    pub fn unused_node_id(&self) -> NodeId {
        NodeId(next_node_id(&self.bmg).0.max(self.first_new.1.0))
    }

    /// Checks `attributes` for width and copies `public_id` over the id in
    /// them, where they hold one.
    fn fit(&self, public_id: u16, attributes: &mut [u8]) -> Result<(), EditError> {
        let expected = self.tables.attributes_len;
        let actual = attributes.len();
        if actual != expected {
            return Err(EditError::AttributeWidth { expected, actual });
        }
        self.tables.set_id(attributes, public_id);
        Ok(())
    }

    fn position(&self, id: MessageId) -> Result<usize, EditError> {
        self.bmg
            .messages
            .iter()
            .position(|message| message.id == id)
            .ok_or(EditError::UnknownMessage(id))
    }

    fn message_mut(&mut self, id: MessageId) -> Result<&mut Message, EditError> {
        self.bmg
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .ok_or(EditError::UnknownMessage(id))
    }

    fn flow_mut(&mut self) -> Result<&mut Flow, EditError> {
        self.bmg.flow.as_mut().ok_or(EditError::NoFlow)
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, EditError> {
        self.flow_mut()?
            .nodes
            .iter_mut()
            .find(|node| node.id() == id)
            .ok_or(EditError::UnknownNode(id))
    }

    fn answers_mut(&mut self, node: NodeId) -> Result<&mut Vec<Option<NodeId>>, EditError> {
        match self.node_mut(node)? {
            Node::Branch { children, .. } => Ok(children),
            Node::Text { .. } | Node::Event { .. } => Err(EditError::WrongKind {
                node,
                expected: "branch",
            }),
        }
    }

    /// `target` must be a node that exists, or `from` itself.
    fn check_target(&self, from: NodeId, target: Option<NodeId>) -> Result<(), EditError> {
        let nodes = self.bmg.flow.as_ref().map_or(&[][..], |flow| &flow.nodes);
        match target {
            Some(target) if target != from && !nodes.iter().any(|node| node.id() == target) => {
                Err(EditError::UnknownNode(target))
            }
            _ => Ok(()),
        }
    }

    /// Every message and node `node` points at must exist, `node` itself
    /// aside, which may point at itself.
    fn check_targets(&self, node: &Node) -> Result<(), EditError> {
        if let Node::Text { message, .. } = node {
            self.position(*message)?;
        }
        edges(node).try_for_each(|target| self.check_target(node.id(), Some(target)))
    }

    fn perform_message(&mut self, edit: MessageEdit) -> Result<MessageEdit, EditError> {
        match edit {
            MessageEdit::Set { id, change } => {
                let change = self.change_message(id, change)?;
                Ok(MessageEdit::Set { id, change })
            }
            MessageEdit::Insert { at, message } => self.insert_message(at, message),
            MessageEdit::Remove(id) => self.remove_message(id),
        }
    }

    fn change_message(
        &mut self,
        id: MessageId,
        change: MessageChange,
    ) -> Result<MessageChange, EditError> {
        match change {
            MessageChange::Text(text) => self.set_text(id, text),
            MessageChange::Field { field, value } => self.set_field(id, field, value),
            MessageChange::Attributes(attributes) => self.set_attributes(id, attributes),
            MessageChange::PublicId(public_id) => self.set_public_id(id, public_id),
        }
    }

    fn set_text(
        &mut self,
        message: MessageId,
        text: Vec<TextSegment>,
    ) -> Result<MessageChange, EditError> {
        let slot = &mut self.message_mut(message)?.text;
        Ok(MessageChange::Text(mem::replace(slot, text)))
    }

    fn set_field(
        &mut self,
        message: MessageId,
        field: Field,
        value: u16,
    ) -> Result<MessageChange, EditError> {
        field_bytes(&field).ok_or(EditError::FieldOutOfRange(field.offset))?;
        if self.tables.is_id(&field) {
            return Err(EditError::IdField);
        }
        let attributes = &mut self.message_mut(message)?.attributes;
        let old = read_field(attributes, &field).ok_or(EditError::FieldOutOfRange(field.offset))?;
        write_field(attributes, &field, value).ok_or(EditError::ValueTooWide {
            value,
            len: field.len,
        })?;
        Ok(MessageChange::Field { field, value: old })
    }

    fn set_attributes(
        &mut self,
        message: MessageId,
        mut attributes: Box<[u8]>,
    ) -> Result<MessageChange, EditError> {
        let public_id = self
            .message(message)
            .ok_or(EditError::UnknownMessage(message))?
            .public_id;
        self.fit(public_id, &mut attributes)?;
        let slot = &mut self.message_mut(message)?.attributes;
        Ok(MessageChange::Attributes(mem::replace(slot, attributes)))
    }

    fn set_public_id(
        &mut self,
        message: MessageId,
        public_id: u16,
    ) -> Result<MessageChange, EditError> {
        let tables = self.tables;
        let slot = self.message_mut(message)?;
        tables.set_id(&mut slot.attributes, public_id);
        Ok(MessageChange::PublicId(mem::replace(
            &mut slot.public_id,
            public_id,
        )))
    }

    fn insert_message(
        &mut self,
        at: usize,
        mut message: Message,
    ) -> Result<MessageEdit, EditError> {
        self.fit(message.public_id, &mut message.attributes)?;
        if self.message(message.id).is_some() {
            return Err(EditError::DuplicateMessage(message.id));
        }
        if at > self.bmg.messages.len() {
            return Err(EditError::OutOfRange(at));
        }
        let id = message.id;
        self.bmg.messages.insert(at, message);
        Ok(MessageEdit::Remove(id))
    }

    fn remove_message(&mut self, id: MessageId) -> Result<MessageEdit, EditError> {
        let at = self.position(id)?;
        let shown = self
            .bmg
            .flow
            .iter()
            .flat_map(|flow| &flow.nodes)
            .any(|node| matches!(node, Node::Text { message, .. } if *message == id));
        if shown {
            return Err(EditError::MessageInUse(id));
        }
        let message = self.bmg.messages.remove(at);
        Ok(MessageEdit::Insert { at, message })
    }

    fn perform_flow(&mut self, edit: FlowEdit) -> Result<FlowEdit, EditError> {
        match edit {
            FlowEdit::Create => self.create_flow(),
            FlowEdit::Delete => self.delete_flow(),
            FlowEdit::Node(edit) => self.perform_node(edit).map(FlowEdit::Node),
            FlowEdit::Root(edit) => self.perform_root(edit).map(FlowEdit::Root),
        }
    }

    fn create_flow(&mut self) -> Result<FlowEdit, EditError> {
        if self.bmg.flow.is_some() {
            return Err(EditError::HasFlow);
        }
        self.bmg.flow = Some(Flow::default());
        Ok(FlowEdit::Delete)
    }

    fn delete_flow(&mut self) -> Result<FlowEdit, EditError> {
        let flow = self.bmg.flow.as_ref().ok_or(EditError::NoFlow)?;
        if !flow.nodes.is_empty() || !flow.roots.is_empty() {
            return Err(EditError::FlowNotEmpty);
        }
        self.bmg.flow = None;
        Ok(FlowEdit::Create)
    }

    fn perform_node(&mut self, edit: NodeEdit) -> Result<NodeEdit, EditError> {
        match edit {
            NodeEdit::Set { id, change } => {
                let change = self.change_node(id, change)?;
                Ok(NodeEdit::Set { id, change })
            }
            NodeEdit::Insert { at, node } => self.insert_node(at, node),
            NodeEdit::Remove(id) => self.remove_node(id),
        }
    }

    fn change_node(&mut self, id: NodeId, change: NodeChange) -> Result<NodeChange, EditError> {
        match change {
            NodeChange::Shown(message) => self.set_shown(id, message),
            NodeChange::Next(next) => self.set_next(id, next),
            NodeChange::Query { query, param } => self.set_query(id, query, param),
            NodeChange::Event { event, params } => self.set_event(id, event, params),
            NodeChange::Answer(edit) => self.perform_answer(id, edit).map(NodeChange::Answer),
        }
    }

    fn set_shown(&mut self, node: NodeId, message: MessageId) -> Result<NodeChange, EditError> {
        self.position(message)?;
        match self.node_mut(node)? {
            Node::Text { message: shown, .. } => {
                Ok(NodeChange::Shown(mem::replace(shown, message)))
            }
            Node::Branch { .. } | Node::Event { .. } => Err(EditError::WrongKind {
                node,
                expected: "text",
            }),
        }
    }

    fn set_next(&mut self, node: NodeId, next: Option<NodeId>) -> Result<NodeChange, EditError> {
        self.check_target(node, next)?;
        match self.node_mut(node)? {
            Node::Text { next: slot, .. } | Node::Event { next: slot, .. } => {
                Ok(NodeChange::Next(mem::replace(slot, next)))
            }
            Node::Branch { .. } => Err(EditError::WrongKind {
                node,
                expected: "text or event",
            }),
        }
    }

    fn set_query(&mut self, node: NodeId, query: u16, param: u16) -> Result<NodeChange, EditError> {
        match self.node_mut(node)? {
            Node::Branch {
                query: query_slot,
                param: param_slot,
                ..
            } => Ok(NodeChange::Query {
                query: mem::replace(query_slot, query),
                param: mem::replace(param_slot, param),
            }),
            Node::Text { .. } | Node::Event { .. } => Err(EditError::WrongKind {
                node,
                expected: "branch",
            }),
        }
    }

    fn set_event(
        &mut self,
        node: NodeId,
        event: u8,
        params: [u8; 4],
    ) -> Result<NodeChange, EditError> {
        match self.node_mut(node)? {
            Node::Event {
                event: event_slot,
                params: params_slot,
                ..
            } => Ok(NodeChange::Event {
                event: mem::replace(event_slot, event),
                params: mem::replace(params_slot, params),
            }),
            Node::Text { .. } | Node::Branch { .. } => Err(EditError::WrongKind {
                node,
                expected: "event",
            }),
        }
    }

    fn perform_answer(
        &mut self,
        node: NodeId,
        edit: ListEdit<Option<NodeId>>,
    ) -> Result<ListEdit<Option<NodeId>>, EditError> {
        match edit {
            ListEdit::Set { at, value } => {
                self.check_target(node, value)?;
                let slot = self
                    .answers_mut(node)?
                    .get_mut(at)
                    .ok_or(EditError::OutOfRange(at))?;
                Ok(ListEdit::Set {
                    at,
                    value: mem::replace(slot, value),
                })
            }
            ListEdit::Insert { at, value } => {
                self.check_target(node, value)?;
                let answers = self.answers_mut(node)?;
                if at > answers.len() {
                    return Err(EditError::OutOfRange(at));
                }
                answers.insert(at, value);
                Ok(ListEdit::Remove { at })
            }
            ListEdit::Remove { at } => {
                let answers = self.answers_mut(node)?;
                if at >= answers.len() {
                    return Err(EditError::OutOfRange(at));
                }
                Ok(ListEdit::Insert {
                    at,
                    value: answers.remove(at),
                })
            }
        }
    }

    fn insert_node(&mut self, at: usize, node: Node) -> Result<NodeEdit, EditError> {
        self.check_targets(&node)?;
        let id = node.id();
        let nodes = &mut self.flow_mut()?.nodes;
        if nodes.iter().any(|other| other.id() == id) {
            return Err(EditError::DuplicateNode(id));
        }
        if at > nodes.len() {
            return Err(EditError::OutOfRange(at));
        }
        nodes.insert(at, node);
        Ok(NodeEdit::Remove(id))
    }

    fn remove_node(&mut self, id: NodeId) -> Result<NodeEdit, EditError> {
        let flow = self.flow_mut()?;
        let at = flow
            .nodes
            .iter()
            .position(|node| node.id() == id)
            .ok_or(EditError::UnknownNode(id))?;
        let reached = flow
            .nodes
            .iter()
            .filter(|node| node.id() != id)
            .flat_map(edges)
            .chain(flow.roots.iter().map(|root| root.node))
            .any(|target| target == id);
        if reached {
            return Err(EditError::NodeInUse(id));
        }
        let node = flow.nodes.remove(at);
        Ok(NodeEdit::Insert { at, node })
    }

    fn check_root(&mut self, root: Root) -> Result<&mut Vec<Root>, EditError> {
        let flow = self.flow_mut()?;
        if !flow.nodes.iter().any(|node| node.id() == root.node) {
            return Err(EditError::UnknownNode(root.node));
        }
        Ok(&mut flow.roots)
    }

    fn perform_root(&mut self, edit: ListEdit<Root>) -> Result<ListEdit<Root>, EditError> {
        match edit {
            ListEdit::Set { at, value } => {
                let slot = self
                    .check_root(value)?
                    .get_mut(at)
                    .ok_or(EditError::OutOfRange(at))?;
                Ok(ListEdit::Set {
                    at,
                    value: mem::replace(slot, value),
                })
            }
            ListEdit::Insert { at, value } => {
                let roots = self.check_root(value)?;
                if at > roots.len() {
                    return Err(EditError::OutOfRange(at));
                }
                roots.insert(at, value);
                Ok(ListEdit::Remove { at })
            }
            ListEdit::Remove { at } => {
                let roots = &mut self.flow_mut()?.roots;
                if at >= roots.len() {
                    return Err(EditError::OutOfRange(at));
                }
                Ok(ListEdit::Insert {
                    at,
                    value: roots.remove(at),
                })
            }
        }
    }
}

impl Editable for EditableBmg {
    type Edit = BmgEdit;
    type Error = EditError;

    fn perform(&mut self, edit: BmgEdit) -> Result<BmgEdit, EditError> {
        match edit {
            BmgEdit::Message(edit) => self.perform_message(edit).map(BmgEdit::Message),
            BmgEdit::Flow(edit) => self.perform_flow(edit).map(BmgEdit::Flow),
        }
    }
}

/// The id past every message `bmg` holds.
pub(crate) fn next_message_id(bmg: &Bmg) -> MessageId {
    let highest = bmg.messages.iter().map(|message| message.id.0).max();
    MessageId(highest.map_or(0, |id| id + 1))
}

/// The id past every node `bmg` holds.
pub(crate) fn next_node_id(bmg: &Bmg) -> NodeId {
    let nodes = bmg.flow.iter().flat_map(|flow| &flow.nodes);
    let highest = nodes.map(|node| node.id().0).max();
    NodeId(highest.map_or(0, |id| id + 1))
}

/// Every node `node` leads to, dead ends left out.
fn edges(node: &Node) -> impl Iterator<Item = NodeId> + '_ {
    let (single, children) = match node {
        Node::Text { next, .. } | Node::Event { next, .. } => (*next, &[][..]),
        Node::Branch { children, .. } => (None, children.as_slice()),
    };
    single.into_iter().chain(children.iter().flatten().copied())
}

#[cfg(test)]
mod tests {
    use tpmt_message::{Encoding, Mid1Header, TEXT_OFFSET_LEN};
    use tpmt_tables::Version;
    use tpmt_tables::message::{record, unit};

    use super::*;
    use crate::History;

    const EDITION: Edition = Edition::default_language(Version::GcnUsa);

    fn message(id: u32, text: &[u8]) -> Message {
        let public_id = u16::try_from(id).unwrap();
        let mut attributes = [0; 16];
        attributes[..2].copy_from_slice(&public_id.to_be_bytes());
        Message {
            public_id,
            id: MessageId(id),
            attributes: Box::new(attributes),
            text: vec![TextSegment::Text(text.into())],
        }
    }

    fn text(text: &[u8]) -> Vec<TextSegment> {
        vec![TextSegment::Text(text.into())]
    }

    fn field(offset: usize) -> Field {
        *record::LAYOUT
            .fields
            .iter()
            .find(|field| field.offset == offset)
            .unwrap()
    }

    fn set_message(id: MessageId, change: MessageChange) -> BmgEdit {
        MessageEdit::Set { id, change }.into()
    }

    fn set_node(id: NodeId, change: NodeChange) -> BmgEdit {
        NodeEdit::Set { id, change }.into()
    }

    fn answer(node: NodeId, edit: ListEdit<Option<NodeId>>) -> BmgEdit {
        set_node(node, NodeChange::Answer(edit))
    }

    fn root(edit: ListEdit<Root>) -> BmgEdit {
        FlowEdit::Root(edit).into()
    }

    /// Two messages, and a flow whose one root shows the first.
    fn file() -> EditableBmg {
        EditableBmg::new(
            Bmg {
                encoding: Encoding::ShiftJis,
                record_len: record::LAYOUT.len,
                mid1: Some(Mid1Header::default()),
                messages: vec![message(0, b"Hello"), message(1, b"Unused")],
                flow: Some(Flow {
                    nodes: vec![Node::Text {
                        id: NodeId(0),
                        message: MessageId(0),
                        next: None,
                    }],
                    roots: vec![Root {
                        public_id: 1,
                        node: NodeId(0),
                    }],
                }),
                strings: None,
            },
            EDITION,
        )
    }

    /// A unit file shaped like `zel_unit.bmg`, from `edition`: records
    /// `record_len` wide, no MID1 or flow, and a string pool.
    fn unit_file(edition: Edition, record_len: u16) -> EditableBmg {
        let attributes = usize::from(record_len - TEXT_OFFSET_LEN);
        EditableBmg::new(
            Bmg {
                encoding: Encoding::ShiftJis,
                record_len,
                mid1: None,
                messages: vec![Message {
                    public_id: 0,
                    id: MessageId(0),
                    attributes: vec![0; attributes].into(),
                    text: Vec::new(),
                }],
                flow: None,
                strings: Some(vec![
                    Box::default(),
                    Box::from(*b"arrow"),
                    Box::from(*b"arrows"),
                ]),
            },
            edition,
        )
    }

    #[test]
    fn undo_and_redo_walk_the_edits_back_and_forth() {
        let original = file();
        let mut file = original.clone();
        let mut history = History::default();

        history
            .apply(
                &mut file,
                set_message(MessageId(0), MessageChange::Text(text(b"Goodbye"))),
            )
            .unwrap();
        history
            .apply(&mut file, MessageEdit::Remove(MessageId(1)).into())
            .unwrap();
        let edited = file.clone();
        assert_eq!(file.bmg().messages.len(), 1);

        assert!(history.undo(&mut file).unwrap());
        assert!(history.undo(&mut file).unwrap());
        // Can't undo any further
        assert!(!history.undo(&mut file).unwrap());
        assert_eq!(file, original);

        assert!(history.redo(&mut file).unwrap());
        assert!(history.redo(&mut file).unwrap());
        assert_eq!(file, edited);
    }

    #[test]
    fn a_new_edit_drops_the_redo_stack() {
        let mut file = file();
        let mut history = History::default();
        let set_text = |body: &[u8]| set_message(MessageId(0), MessageChange::Text(text(body)));
        history.apply(&mut file, set_text(b"A")).unwrap();
        history.undo(&mut file).unwrap();
        history.apply(&mut file, set_text(b"B")).unwrap();
        assert!(!history.can_redo());
    }

    #[test]
    fn a_refused_edit_changes_nothing() {
        let original = file();
        let mut file = original.clone();
        let mut history = History::default();
        let refused = [
            (
                MessageEdit::Remove(MessageId(0)).into(),
                EditError::MessageInUse(MessageId(0)),
            ),
            (
                NodeEdit::Remove(NodeId(0)).into(),
                EditError::NodeInUse(NodeId(0)),
            ),
            (
                set_message(MessageId(9), MessageChange::Text(text(b"foo"))),
                EditError::UnknownMessage(MessageId(9)),
            ),
            (
                MessageEdit::Insert {
                    at: 0,
                    message: message(1, b""),
                }
                .into(),
                EditError::DuplicateMessage(MessageId(1)),
            ),
            (
                set_message(MessageId(0), MessageChange::Attributes(Box::new([0; 3]))),
                EditError::AttributeWidth {
                    expected: 16,
                    actual: 3,
                },
            ),
            (
                set_message(
                    MessageId(0),
                    MessageChange::Field {
                        field: field(0x09),
                        value: 256,
                    },
                ),
                EditError::ValueTooWide { value: 256, len: 1 },
            ),
            (
                set_message(
                    MessageId(0),
                    MessageChange::Field {
                        field: field(0x04),
                        value: 7,
                    },
                ),
                EditError::IdField,
            ),
            (
                set_node(NodeId(0), NodeChange::Next(Some(NodeId(5)))),
                EditError::UnknownNode(NodeId(5)),
            ),
            (
                set_node(NodeId(0), NodeChange::Query { query: 0, param: 0 }),
                EditError::WrongKind {
                    node: NodeId(0),
                    expected: "branch",
                },
            ),
            (root(ListEdit::Remove { at: 1 }), EditError::OutOfRange(1)),
            (FlowEdit::Delete.into(), EditError::FlowNotEmpty),
            (FlowEdit::Create.into(), EditError::HasFlow),
        ];
        for (edit, error) in refused {
            assert_eq!(history.apply(&mut file, edit), Err(error));
        }
        assert_eq!(file, original);
        assert!(!history.can_undo());
    }

    /// Building a conversation up from nothing, then undoing all of it, gives
    /// back a file with no flow at all rather than an empty one.
    #[test]
    fn a_flow_built_and_undone_leaves_no_flow_behind() {
        let mut file = file();
        let mut history = History::default();
        history
            .apply(&mut file, root(ListEdit::Remove { at: 0 }))
            .unwrap();
        history
            .apply(&mut file, NodeEdit::Remove(NodeId(0)).into())
            .unwrap();
        history.apply(&mut file, FlowEdit::Delete.into()).unwrap();
        let original = file.clone();

        let node = file.unused_node_id();
        history.apply(&mut file, FlowEdit::Create.into()).unwrap();
        let text = Node::Text {
            id: node,
            message: MessageId(1),
            next: Some(node),
        };
        history
            .apply(&mut file, NodeEdit::Insert { at: 0, node: text }.into())
            .unwrap();
        history
            .apply(
                &mut file,
                root(ListEdit::Insert {
                    at: 0,
                    value: Root { public_id: 7, node },
                }),
            )
            .unwrap();
        assert!(file.save().is_ok());

        for _ in 0..3 {
            history.undo(&mut file).unwrap();
        }
        assert_eq!(file, original);
        assert_eq!(file.bmg().flow, None);
    }

    #[test]
    fn a_branch_is_rewired_one_answer_at_a_time() {
        let original = file();
        let mut file = original.clone();
        let mut history = History::default();
        let branch = file.unused_node_id();
        let edits = [
            NodeEdit::Insert {
                at: 1,
                node: Node::Branch {
                    id: branch,
                    query: 0,
                    param: 0,
                    children: vec![None, None],
                },
            }
            .into(),
            answer(
                branch,
                ListEdit::Set {
                    at: 1,
                    value: Some(NodeId(0)),
                },
            ),
            answer(
                branch,
                ListEdit::Insert {
                    at: 2,
                    value: Some(branch),
                },
            ),
            answer(branch, ListEdit::Remove { at: 0 }),
            set_node(branch, NodeChange::Query { query: 3, param: 2 }),
            set_node(NodeId(0), NodeChange::Next(Some(branch))),
        ];
        for edit in edits.clone() {
            history.apply(&mut file, edit).unwrap();
        }
        let flow = file.bmg().flow.as_ref().unwrap();
        assert_eq!(
            flow.nodes[1],
            Node::Branch {
                id: branch,
                query: 3,
                param: 2,
                children: vec![Some(NodeId(0)), Some(branch)],
            }
        );
        assert_eq!(
            history.apply(
                &mut file,
                answer(branch, ListEdit::Set { at: 2, value: None }),
            ),
            Err(EditError::OutOfRange(2))
        );

        for _ in edits {
            history.undo(&mut file).unwrap();
        }
        assert_eq!(file, original);
    }

    #[test]
    fn setting_the_public_id_rewrites_its_copy_in_the_attributes() {
        let mut file = file();
        let mut history = History::default();
        history
            .apply(
                &mut file,
                set_message(MessageId(1), MessageChange::PublicId(0x1234)),
            )
            .unwrap();
        let message = file.message(MessageId(1)).unwrap();
        assert_eq!(
            &message.attributes[field_bytes(&record::ID).unwrap()],
            &[0x12, 0x34]
        );
        assert_eq!(
            EditableBmg::open(&file.save().unwrap(), EDITION).unwrap(),
            file
        );

        history.undo(&mut file).unwrap();
        let message = file.message(MessageId(1)).unwrap();
        assert_eq!(message.public_id, 1);
        assert_eq!(
            &message.attributes[field_bytes(&record::ID).unwrap()],
            &[0, 1]
        );
    }

    #[test]
    fn a_field_edit_undoes_to_the_old_value() {
        let mut file = file();
        let mut history = History::default();
        let box_kind = field(0x09);
        let read = |file: &EditableBmg| {
            read_field(&file.message(MessageId(0)).unwrap().attributes, &box_kind)
        };
        history
            .apply(
                &mut file,
                set_message(
                    MessageId(0),
                    MessageChange::Field {
                        field: box_kind,
                        value: 13,
                    },
                ),
            )
            .unwrap();
        assert_eq!(read(&file), Some(13));
        history.undo(&mut file).unwrap();
        assert_eq!(read(&file), Some(0));
    }

    #[test]
    fn a_record_id_that_disagrees_with_mid1_is_refused() {
        let mut bmg = file().bmg;
        bmg.messages[1].attributes[field_bytes(&record::ID).unwrap()]
            .copy_from_slice(&7u16.to_be_bytes());
        assert!(matches!(
            EditableBmg::open(&bmg.encode().unwrap(), EDITION),
            Err(OpenError::IdMismatch {
                message: MessageId(1),
                public_id: 1,
                record: 7,
            })
        ));

        // Without a MID1 the record's id is the game's to use.
        bmg.mid1 = None;
        assert!(EditableBmg::open(&bmg.encode().unwrap(), EDITION).is_ok());
    }

    #[test]
    fn saving_and_opening_gives_the_same_file() {
        let file = file();
        assert_eq!(
            EditableBmg::open(&file.save().unwrap(), EDITION).unwrap(),
            file
        );
    }

    #[test]
    fn a_unit_file_gets_its_region_layout() {
        for (version, layout) in [
            (Version::GcnUsa, unit::LAYOUT),
            (Version::GcnJpn, unit::JPN_LAYOUT),
        ] {
            let edition = Edition::default_language(version);
            let file = unit_file(edition, layout.len);
            assert_eq!(file.tables().layout, Some(&layout));
            assert_eq!(
                EditableBmg::open(&file.save().unwrap(), edition).unwrap(),
                file
            );
        }
    }

    /// A JPN unit file in a USA project isn't named with the JPN layout.
    #[test]
    fn a_layout_from_another_region_is_not_used() {
        let file = unit_file(EDITION, unit::JPN_LAYOUT.len);
        assert_eq!(file.tables().layout, None);
    }

    /// A unit record's first field sits where the story record keeps its id,
    /// so setting it and the public id must leave each other alone.
    #[test]
    fn a_unit_field_is_not_the_id() {
        let mut file = unit_file(EDITION, unit::LAYOUT.len);
        let mut history = History::default();
        let singular = unit::LAYOUT.fields[0];
        let edits = [
            set_message(
                MessageId(0),
                MessageChange::Field {
                    field: singular,
                    value: 7,
                },
            ),
            set_message(MessageId(0), MessageChange::PublicId(9)),
        ];
        for edit in edits {
            history.apply(&mut file, edit).unwrap();
        }
        let message = file.message(MessageId(0)).unwrap();
        assert_eq!(read_field(&message.attributes, &singular), Some(7));
        assert_eq!(message.public_id, 9);
    }

    #[test]
    fn changes_name_each_record_touched() {
        let mut file = file();
        let vanilla = file.bmg().clone();
        let new = file.unused_message_id();
        let mut history = History::default();
        for edit in [
            set_message(MessageId(0), MessageChange::Text(text(b"Goodbye"))),
            MessageEdit::Remove(MessageId(1)).into(),
            MessageEdit::Insert {
                at: 1,
                message: message(new.0, b"New"),
            }
            .into(),
            root(ListEdit::Insert {
                at: 1,
                value: Root {
                    public_id: 2,
                    node: NodeId(0),
                },
            }),
        ] {
            history.apply(&mut file, edit).unwrap();
        }

        let changes = BmgChanges::new(Some(&vanilla), file.bmg());
        assert_eq!(
            changes.messages,
            [
                (MessageId(0), Status::Changed),
                (MessageId(1), Status::Removed),
                (new, Status::Added),
            ]
        );
        assert_eq!(changes.nodes, [(NodeId(0), Status::Unchanged)]);
        assert_eq!(changes.roots, [(1, Status::Unchanged), (2, Status::Added)]);

        let whole = BmgChanges::new(None, file.bmg());
        assert!(
            whole
                .messages
                .iter()
                .all(|(_, status)| *status == Status::Added)
        );
    }
}
