//! A patch put back over the vanilla file, checked as it goes.

use std::collections::{BTreeMap, HashMap, HashSet};

use tpmt_message::{Bmg, Flow, Message, MessageId, Node, NodeId, Root};
use tpmt_tables::Edition;

use super::ids::Ids;
use super::{
    BmgPatch, MessageKey, MessageRef, Names, NewNode, NodePatch, NodeRef, Number, PatchError,
};
use crate::message::tables::text::unhex;
use crate::message::tables::{Tables, write_field};

/// Puts `patch` over `vanilla`, a fresh decode of the file, and returns
/// the result with the names its new items go by.
///
/// # Errors
///
/// When the patch names something that isn't there, doesn't fit the file,
/// or leaves the flow graph pointing at something removed.
pub fn apply(
    vanilla: &Bmg,
    patch: &BmgPatch,
    edition: Edition,
) -> Result<(Bmg, Names), PatchError> {
    let tables = Tables::new(vanilla, edition);
    let ids = Ids::new(vanilla);
    let mut bmg = vanilla.clone();
    let mut names = Names::default();

    let mut new_messages = HashMap::new();
    for (id, new) in (ids.messages..).zip(&patch.new.message) {
        claim(&new.name, &mut new_messages, MessageId(id))?;
        names.messages.insert(MessageId(id), new.name.clone());
    }
    let mut new_nodes = HashMap::new();
    for (id, new) in (ids.nodes..).zip(&patch.new.node) {
        claim(&new.name, &mut new_nodes, NodeId(id))?;
        names.nodes.insert(NodeId(id), new.name.clone());
    }
    let lookup = Lookup {
        ids: &ids,
        new_messages: &new_messages,
        new_nodes: &new_nodes,
    };

    for (key, change) in &patch.message {
        let id = lookup.vanilla_message(*key)?;
        let message = bmg
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .ok_or(PatchError::UnknownMessage(*key))?;
        let edit = Values {
            public_id: change.public_id,
            fields: &change.fields,
            attributes: change.attributes.as_deref(),
        };
        set_values(
            &tables,
            message,
            &edit,
            change.text.as_deref(),
            &key.to_string(),
        )?;
    }
    for (id, new) in (ids.messages..).zip(&patch.new.message) {
        let mut message = Message {
            public_id: 0,
            id: MessageId(id),
            attributes: vec![0; tables.attributes_len].into(),
            text: Vec::new(),
        };
        let edit = Values {
            public_id: new.public_id,
            fields: &new.fields,
            attributes: new.attributes.as_deref(),
        };
        set_values(&tables, &mut message, &edit, Some(&new.text), &new.name)?;
        bmg.messages.push(message);
    }
    for key in &patch.remove.messages {
        let id = lookup.vanilla_message(*key)?;
        bmg.messages.retain(|message| message.id != id);
    }

    apply_flow(&mut bmg, patch, &lookup)?;
    check_graph(&bmg, &names)?;
    Ok((bmg, names))
}

/// Sets what `edit` and `text` give on `message`. `name` is what errors call
/// it.
fn set_values(
    tables: &Tables,
    message: &mut Message,
    edit: &Values<'_>,
    text: Option<&str>,
    name: &str,
) -> Result<(), PatchError> {
    if let Some(attributes) = edit.attributes {
        let bytes = unhex(attributes).map_err(|error| PatchError::Text {
            key: name.to_string(),
            error,
        })?;
        let expected = tables.attributes_len;
        if bytes.len() != expected {
            return Err(PatchError::AttributeWidth {
                expected,
                actual: bytes.len(),
            });
        }
        message.attributes = bytes.into();
    }
    for (name, value) in edit.fields {
        let field = tables
            .field(name)
            .ok_or_else(|| PatchError::UnknownField(name.clone()))?;
        if tables.is_id(field) {
            return Err(PatchError::IdField(field.name));
        }
        write_field(&mut message.attributes, field, *value).ok_or(PatchError::ValueTooWide {
            field: field.name,
            value: *value,
        })?;
    }
    if let Some(text) = text {
        message.text = tables.parse(text).map_err(|error| PatchError::Text {
            key: name.to_string(),
            error,
        })?;
    }
    if let Some(public_id) = edit.public_id {
        if !tables.has_mid1 {
            return Err(PatchError::NoMid1);
        }
        message.public_id = public_id;
    }
    tables.set_id(&mut message.attributes, message.public_id);
    Ok(())
}

/// What a patch sets in a message's attributes.
struct Values<'a> {
    public_id: Option<u16>,
    fields: &'a BTreeMap<String, u16>,
    attributes: Option<&'a str>,
}

/// Resolves what a patch names into ids.
struct Lookup<'a> {
    ids: &'a Ids,
    new_messages: &'a HashMap<String, MessageId>,
    new_nodes: &'a HashMap<String, NodeId>,
}

impl Lookup<'_> {
    fn vanilla_message(&self, key: MessageKey) -> Result<MessageId, PatchError> {
        self.ids
            .by_key
            .get(&key)
            .copied()
            .ok_or(PatchError::UnknownMessage(key))
    }

    fn message(&self, reference: &MessageRef) -> Result<MessageId, PatchError> {
        match reference {
            MessageRef::Vanilla(key) => self.vanilla_message(*key),
            MessageRef::New(name) => self
                .new_messages
                .get(name)
                .copied()
                .ok_or_else(|| PatchError::UnknownName(name.clone())),
        }
    }

    fn node(&self, reference: &NodeRef) -> Result<Option<NodeId>, PatchError> {
        match reference {
            NodeRef::End => Ok(None),
            NodeRef::Vanilla(at) if *at < self.ids.nodes => Ok(Some(NodeId(*at))),
            NodeRef::Vanilla(at) => Err(PatchError::UnknownNode(*at)),
            NodeRef::New(name) => self
                .new_nodes
                .get(name)
                .copied()
                .map(Some)
                .ok_or_else(|| PatchError::UnknownName(name.clone())),
        }
    }

    fn nodes(&self, references: &[NodeRef]) -> Result<Vec<Option<NodeId>>, PatchError> {
        references
            .iter()
            .map(|reference| self.node(reference))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Branch,
    Event,
}

const fn kind_of(node: &Node) -> Kind {
    match node {
        Node::Text { .. } => Kind::Text,
        Node::Branch { .. } => Kind::Branch,
        Node::Event { .. } => Kind::Event,
    }
}

impl NodePatch {
    /// The kind the fields imply, or `None` when they fit more than one.
    fn kind(&self, name: &str) -> Result<Option<Kind>, PatchError> {
        let text = self.message.is_some();
        let branch = self.query.is_some() || self.param.is_some() || self.answers.is_some();
        let event = self.event.is_some() || self.params.is_some();
        match (text, branch, event) {
            (false, false, false) => Ok(None),
            (true, false, false) => Ok(Some(Kind::Text)),
            (false, true, false) => Ok(Some(Kind::Branch)),
            (false, false, true) => Ok(Some(Kind::Event)),
            _ => Err(PatchError::MixedNode(name.to_string())),
        }
    }

    /// A node of `kind` with id `id` from these fields alone.
    fn build(
        &self,
        id: NodeId,
        kind: Kind,
        name: &str,
        lookup: &Lookup<'_>,
    ) -> Result<Node, PatchError> {
        let missing = |field| PatchError::WrongField {
            node: name.to_string(),
            field,
        };
        let next = self
            .next
            .as_ref()
            .map_or(Ok(None), |next| lookup.node(next))?;
        Ok(match kind {
            Kind::Text => Node::Text {
                id,
                message: lookup
                    .message(self.message.as_ref().ok_or_else(|| missing("message"))?)?,
                next,
            },
            Kind::Branch => {
                if self.next.is_some() {
                    return Err(missing("next"));
                }
                Node::Branch {
                    id,
                    query: self.query.ok_or_else(|| missing("query"))?,
                    param: self.param.unwrap_or(0),
                    children: lookup.nodes(self.answers.as_deref().unwrap_or_default())?,
                }
            }
            Kind::Event => Node::Event {
                id,
                event: self.event.ok_or_else(|| missing("event"))?,
                params: self.params.unwrap_or_default(),
                next,
            },
        })
    }

    /// Sets these fields on `node`, or replaces it when they imply another
    /// kind.
    fn patch(&self, node: &mut Node, name: &str, lookup: &Lookup<'_>) -> Result<(), PatchError> {
        let kind = self.kind(name)?.unwrap_or_else(|| kind_of(node));
        if kind != kind_of(node) {
            *node = self.build(node.id(), kind, name, lookup)?;
            return Ok(());
        }
        let wrong = |field| PatchError::WrongField {
            node: name.to_string(),
            field,
        };
        match node {
            Node::Text { message, next, .. } => {
                if let Some(shown) = &self.message {
                    *message = lookup.message(shown)?;
                }
                if let Some(to) = &self.next {
                    *next = lookup.node(to)?;
                }
            }
            Node::Branch {
                query,
                param,
                children,
                ..
            } => {
                if self.next.is_some() {
                    return Err(wrong("next"));
                }
                *query = self.query.unwrap_or(*query);
                *param = self.param.unwrap_or(*param);
                if let Some(answers) = &self.answers {
                    *children = lookup.nodes(answers)?;
                }
            }
            Node::Event {
                event,
                params,
                next,
                ..
            } => {
                *event = self.event.unwrap_or(*event);
                *params = self.params.unwrap_or(*params);
                if let Some(to) = &self.next {
                    *next = lookup.node(to)?;
                }
            }
        }
        Ok(())
    }
}

impl From<&NewNode> for NodePatch {
    fn from(new: &NewNode) -> Self {
        Self {
            message: new.message.clone(),
            query: new.query,
            param: new.param,
            answers: new.answers.clone(),
            event: new.event,
            params: new.params,
            next: new.next.clone(),
        }
    }
}

fn apply_flow(bmg: &mut Bmg, patch: &BmgPatch, lookup: &Lookup<'_>) -> Result<(), PatchError> {
    let touches = !patch.node.is_empty()
        || !patch.root.is_empty()
        || !patch.new.node.is_empty()
        || patch.new.flow
        || !patch.remove.nodes.is_empty()
        || !patch.remove.roots.is_empty();
    if patch.remove.flow {
        if touches {
            return Err(PatchError::RemovedFlow);
        }
        bmg.flow.take().ok_or(PatchError::NoFlow)?;
        return Ok(());
    }
    if patch.new.flow {
        if bmg.flow.is_some() {
            return Err(PatchError::HasFlow);
        }
        bmg.flow = Some(Flow::default());
    }
    if !touches {
        return Ok(());
    }
    let flow = bmg.flow.as_mut().ok_or(PatchError::NoFlow)?;

    for (Number(at), change) in &patch.node {
        let node = flow
            .nodes
            .iter_mut()
            .find(|node| node.id() == NodeId(*at))
            .ok_or(PatchError::UnknownNode(*at))?;
        change.patch(node, &NodeRef::Vanilla(*at).to_string(), lookup)?;
    }
    for (id, new) in (lookup.ids.nodes..).zip(&patch.new.node) {
        let fields = NodePatch::from(new);
        let kind = fields
            .kind(&new.name)?
            .ok_or_else(|| PatchError::NoKind(new.name.clone()))?;
        flow.nodes
            .push(fields.build(NodeId(id), kind, &new.name, lookup)?);
    }
    for at in &patch.remove.nodes {
        let before = flow.nodes.len();
        flow.nodes.retain(|node| node.id() != NodeId(*at));
        if flow.nodes.len() == before {
            return Err(PatchError::UnknownNode(*at));
        }
    }

    for (Number(public_id), to) in &patch.root {
        let node = lookup.node(to)?.ok_or(PatchError::EndRoot)?;
        match flow
            .roots
            .iter_mut()
            .find(|root| root.public_id == *public_id)
        {
            Some(root) => root.node = node,
            None => flow.roots.push(Root {
                public_id: *public_id,
                node,
            }),
        }
    }
    for public_id in &patch.remove.roots {
        let at = flow
            .roots
            .iter()
            .position(|root| root.public_id == *public_id)
            .ok_or(PatchError::UnknownRoot(*public_id))?;
        flow.roots.remove(at);
    }
    Ok(())
}

/// Every message a node shows and every node an edge or root reaches must
/// still be there.
fn check_graph(bmg: &Bmg, names: &Names) -> Result<(), PatchError> {
    let Some(flow) = &bmg.flow else {
        return Ok(());
    };
    let describe = |id: NodeId| {
        names
            .nodes
            .get(&id)
            .map_or_else(|| NodeRef::Vanilla(id.0).to_string(), Clone::clone)
    };
    let messages: HashSet<MessageId> = bmg.messages.iter().map(|message| message.id).collect();
    let nodes: HashSet<NodeId> = flow.nodes.iter().map(Node::id).collect();
    let reach = |from: String, to: NodeId| {
        if nodes.contains(&to) {
            Ok(())
        } else {
            Err(PatchError::RemovedNode {
                from,
                to: describe(to),
            })
        }
    };
    for node in &flow.nodes {
        let (shown, next, children) = match node {
            Node::Text { message, next, .. } => (Some(*message), *next, &[][..]),
            Node::Branch { children, .. } => (None, None, children.as_slice()),
            Node::Event { next, .. } => (None, *next, &[][..]),
        };
        if shown.is_some_and(|message| !messages.contains(&message)) {
            return Err(PatchError::RemovedMessage {
                node: describe(node.id()),
            });
        }
        for to in next.iter().chain(children.iter().flatten()) {
            reach(describe(node.id()), *to)?;
        }
    }
    for root in &flow.roots {
        reach(format!("root {}", root.public_id), root.node)?;
    }
    Ok(())
}

/// Takes `name` for `id`, refusing a bad or repeated one.
fn claim<T>(name: &str, taken: &mut HashMap<String, T>, id: T) -> Result<(), PatchError> {
    let bad = name.is_empty()
        || name == "end"
        || name.contains(':')
        || name.starts_with(|first: char| first.is_ascii_digit() || first == '@');
    if bad {
        return Err(PatchError::BadName(name.to_string()));
    }
    if taken.insert(name.to_string(), id).is_some() {
        return Err(PatchError::DuplicateName(name.to_string()));
    }
    Ok(())
}
