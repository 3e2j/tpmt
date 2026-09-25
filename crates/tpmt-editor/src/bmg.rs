//! A BMG message file as an editable document.
//!
//! Text is kept as `jmessage`'s segments, with tags whole, so nothing is
//! re-parsed on save.
//!
//! Edits are checked for what would leave the graph pointing at nothing: a
//! removed message a text node still shows, a removed node an edge still
//! reaches. Malformed text is left to [`BmgDocument::save`], which refuses
//! it the same way the encoder does.

use std::mem;
use std::ops::Range;

use tpmt_format::Format;
use tpmt_game::bmg::record::{self, Field};
use tpmt_game::bmg::tag::{self, Tag};
use tpmt_jmessage::{Bmg, Flow, Message, MessageId, Node, NodeId, Root, TextSegment};

use crate::Document;

/// The text offset at the front of every INF1 record, which a message's
/// attributes leave out.
const TEXT_OFFSET_LEN: usize = 4;

/// Where a file with a MID1 repeats each message's id in its attributes.
const RECORD_ID: Range<usize> = 0..2;

/// What opens every tag.
const TAG_OPENER: u8 = 0x1A;

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
    Attributes(Vec<u8>),
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

/// A message file open for editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmgDocument {
    bmg: Bmg,
}

impl BmgDocument {
    /// # Errors
    ///
    /// When the bytes aren't a BMG, or are a broken one.
    pub fn open(bytes: &[u8]) -> Result<Self, tpmt_jmessage::Error> {
        Bmg::decode(bytes).map(Self::from)
    }

    /// The file as bytes, for the pipeline to write into the overlay.
    ///
    /// # Errors
    ///
    /// When the encoder refuses the file, such as a text run holding a tag
    /// opener. See [`Bmg::encode`].
    pub fn save(&self) -> Result<Vec<u8>, tpmt_jmessage::Error> {
        self.bmg.encode()
    }

    #[must_use]
    pub const fn bmg(&self) -> &Bmg {
        &self.bmg
    }

    #[must_use]
    pub fn message(&self, id: MessageId) -> Option<&Message> {
        self.bmg.messages.iter().find(|message| message.id == id)
    }

    /// The named fields of this file's records, or `None` when its records
    /// aren't the game's 20-byte message record (`zel_unit.bmg`).
    #[must_use]
    pub fn fields(&self) -> Option<&'static [Field]> {
        (self.bmg.record_len == record::LEN).then_some(record::FIELDS)
    }

    /// An internal id no message holds yet, for [`MessageEdit::Insert`].
    #[must_use]
    pub fn unused_message_id(&self) -> MessageId {
        let highest = self.bmg.messages.iter().map(|message| message.id.0).max();
        MessageId(highest.map_or(0, |id| id + 1))
    }

    /// A node id no node holds yet, for [`NodeEdit::Insert`].
    #[must_use]
    pub fn unused_node_id(&self) -> NodeId {
        let highest = self
            .bmg
            .flow
            .iter()
            .flat_map(|flow| &flow.nodes)
            .map(|node| node.id().0)
            .max();
        NodeId(highest.map_or(0, |id| id + 1))
    }

    fn attributes_len(&self) -> usize {
        usize::from(self.bmg.record_len).saturating_sub(TEXT_OFFSET_LEN)
    }

    /// Checks `attributes` for width and, in a file with a MID1, copies
    /// `public_id` over the id in them, as the encoder does. What's left is
    /// what the file would hold after a save and reopen.
    fn fit(&self, public_id: u16, attributes: &mut [u8]) -> Result<(), EditError> {
        let expected = self.attributes_len();
        let actual = attributes.len();
        if actual != expected {
            return Err(EditError::AttributeWidth { expected, actual });
        }
        if self.bmg.mid1.is_some()
            && let Some(id) = attributes.get_mut(RECORD_ID)
        {
            id.copy_from_slice(&public_id.to_be_bytes());
        }
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
        let bytes = field_bytes(&field).ok_or(EditError::FieldOutOfRange(field.offset))?;
        if self.bmg.mid1.is_some() && bytes.start < RECORD_ID.end && RECORD_ID.start < bytes.end {
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
        mut attributes: Vec<u8>,
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
        let has_mid1 = self.bmg.mid1.is_some();
        let slot = self.message_mut(message)?;
        if has_mid1 && let Some(id) = slot.attributes.get_mut(RECORD_ID) {
            id.copy_from_slice(&public_id.to_be_bytes());
        }
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

impl From<Bmg> for BmgDocument {
    fn from(bmg: Bmg) -> Self {
        Self { bmg }
    }
}

impl Document for BmgDocument {
    type Edit = BmgEdit;
    type Error = EditError;

    fn perform(&mut self, edit: BmgEdit) -> Result<BmgEdit, EditError> {
        match edit {
            BmgEdit::Message(edit) => self.perform_message(edit).map(BmgEdit::Message),
            BmgEdit::Flow(edit) => self.perform_flow(edit).map(BmgEdit::Flow),
        }
    }
}

/// Every node `node` leads to, dead ends left out.
fn edges(node: &Node) -> impl Iterator<Item = NodeId> + '_ {
    let (single, children) = match node {
        Node::Text { next, .. } | Node::Event { next, .. } => (*next, &[][..]),
        Node::Branch { children, .. } => (None, children.as_slice()),
    };
    single.into_iter().chain(children.iter().flatten().copied())
}

/// Where a record field sits in a message's attributes, or `None` when it
/// sits in the text offset.
fn field_bytes(field: &Field) -> Option<Range<usize>> {
    let at = field.offset.checked_sub(TEXT_OFFSET_LEN)?;
    Some(at..at + field.len)
}

/// A record field's value out of a message's attributes, or `None` when the
/// attributes are too short to hold it.
#[must_use]
pub fn read_field(attributes: &[u8], field: &Field) -> Option<u16> {
    match attributes.get(field_bytes(field)?)? {
        [byte] => Some(u16::from(*byte)),
        [high, low] => Some(u16::from_be_bytes([*high, *low])),
        _ => None,
    }
}

/// Writes a record field's value into a message's attributes. `None` when the
/// attributes are too short, or `value` doesn't fit a 1-byte field.
///
/// The message id field is overwritten from [`Message::public_id`] on save in
/// a file with a MID1, so set that instead.
#[must_use]
pub fn write_field(attributes: &mut [u8], field: &Field, value: u16) -> Option<()> {
    match attributes.get_mut(field_bytes(field)?)? {
        [byte] => *byte = u8::try_from(value).ok()?,
        [high, low] => [*high, *low] = value.to_be_bytes(),
        _ => return None,
    }
    Some(())
}

/// A tag's parts, as `JMessage::TProcessor::on_tag_` reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagParts<'a> {
    pub group: u8,
    pub code: u16,
    pub args: &'a [u8],
}

impl TagParts<'_> {
    /// What the game does with the tag, or `None` when nothing names it.
    #[must_use]
    pub fn kind(&self) -> Option<&'static Tag> {
        tag::find(self.group, self.code)
    }
}

/// Splits a whole tag, as [`tpmt_jmessage::TextSegment::Tag`] holds it.
/// `None` when it is too short to have a group and code.
#[must_use]
pub const fn split_tag(tag: &[u8]) -> Option<TagParts<'_>> {
    match tag {
        [TAG_OPENER, _len, group, high, low, args @ ..] => Some(TagParts {
            group: *group,
            code: u16::from_be_bytes([*high, *low]),
            args,
        }),
        _ => None,
    }
}
