//! A patch put back over the vanilla file, checked as it goes.

use std::collections::{BTreeMap, HashMap, HashSet};

use tpmt_message::{Bmg, Flow, Message, MessageId, Node, NodeId, Root};
use tpmt_report::Diagnostic;
use tpmt_tables::Edition;

use super::ids::Ids;
use super::{
    BmgPatch, Entry, MessageKey, MessageRef, Names, NewNode, NodePatch, NodeRef, Number,
    PatchDiagnostic, PatchError,
};
use crate::message::tables::text::unhex;
use crate::message::tables::{Tables, write_field};

/// Puts `patch` over `vanilla`, a fresh decode of the file, and returns
/// the result with the names its new items go by.
///
/// # Errors
///
/// Every entry that names something that isn't there or doesn't fit the
/// file. Once every entry applies, every node left pointing at something
/// removed. A bad entry is skipped, so it can't cause errors further on.
pub fn apply(
    vanilla: &Bmg,
    patch: &BmgPatch,
    edition: Edition,
) -> Result<(Bmg, Names), Vec<PatchDiagnostic>> {
    let tables = Tables::new(vanilla, edition);
    let ids = Ids::new(vanilla);
    let mut bmg = vanilla.clone();
    let mut names = Names::default();
    let mut found = Vec::new();

    let mut new_messages = HashMap::new();
    for (id, new) in (ids.messages..).zip(&patch.new.message) {
        if let Err(error) = claim(&new.name, &mut new_messages, MessageId(id)) {
            found.push(Diagnostic::error(
                error,
                Entry::NewMessage(new.name.clone()),
            ));
        }
        names.messages.insert(MessageId(id), new.name.clone());
    }
    let mut new_nodes = HashMap::new();
    for (id, new) in (ids.nodes..).zip(&patch.new.node) {
        if let Err(error) = claim(&new.name, &mut new_nodes, NodeId(id)) {
            found.push(Diagnostic::error(error, Entry::NewNode(new.name.clone())));
        }
        names.nodes.insert(NodeId(id), new.name.clone());
    }
    let lookup = Lookup {
        ids: &ids,
        new_messages: &new_messages,
        new_nodes: &new_nodes,
    };

    for (key, change) in &patch.message {
        let entry = Entry::Message(*key);
        let message = lookup
            .vanilla_message(*key)
            .ok()
            .and_then(|id| bmg.messages.iter_mut().find(|message| message.id == id));
        let Some(message) = message else {
            found.push(Diagnostic::error(PatchError::UnknownMessage(*key), entry));
            continue;
        };
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
            &mut |error| {
                found.push(Diagnostic::error(error, entry.clone()));
            },
        );
    }
    for (id, new) in (ids.messages..).zip(&patch.new.message) {
        let entry = Entry::NewMessage(new.name.clone());
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
        set_values(
            &tables,
            &mut message,
            &edit,
            Some(&new.text),
            &mut |error| {
                found.push(Diagnostic::error(error, entry.clone()));
            },
        );
        bmg.messages.push(message);
    }
    for key in &patch.remove.messages {
        match lookup.vanilla_message(*key) {
            Ok(id) => bmg.messages.retain(|message| message.id != id),
            Err(error) => found.push(Diagnostic::error(error, Entry::Remove)),
        }
    }

    apply_flow(&mut bmg, patch, &lookup, &mut found);
    // A skipped entry leaves the graph short of whatever it added, so
    // checking it then would only blame the entries that point there.
    if found.is_empty() {
        check_graph(&bmg, &names, &mut found);
    }
    if found.is_empty() {
        Ok((bmg, names))
    } else {
        Err(found)
    }
}

/// Sets what `edit` and `text` give on `message`, handing each problem to
/// `found` and going on with the rest.
fn set_values(
    tables: &Tables,
    message: &mut Message,
    edit: &Values<'_>,
    text: Option<&str>,
    found: &mut impl FnMut(PatchError),
) {
    if let Some(attributes) = edit.attributes {
        let expected = tables.attributes_len;
        match unhex(attributes) {
            Err(error) => found(PatchError::Text(error)),
            Ok(bytes) if bytes.len() != expected => found(PatchError::AttributeWidth {
                expected,
                actual: bytes.len(),
            }),
            Ok(bytes) => message.attributes = bytes.into(),
        }
    }
    for (name, value) in edit.fields {
        let Some(field) = tables.field(name) else {
            found(PatchError::UnknownField(name.clone()));
            continue;
        };
        if tables.is_id(field) {
            found(PatchError::IdField(field.name));
        } else if write_field(&mut message.attributes, field, *value).is_none() {
            found(PatchError::ValueTooWide {
                field: field.name,
                value: *value,
            });
        }
    }
    if let Some(text) = text {
        match tables.parse(text) {
            Ok(text) => message.text = text,
            Err(errors) => errors
                .into_iter()
                .for_each(|error| found(PatchError::Text(error.code))),
        }
    }
    if let Some(public_id) = edit.public_id {
        if tables.has_mid1 {
            message.public_id = public_id;
        } else {
            found(PatchError::NoMid1);
        }
    }
    tables.set_id(&mut message.attributes, message.public_id);
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

/// What a node patch's references name, every one found.
struct Resolved {
    message: Option<MessageId>,
    next: Option<Edge>,
    answers: Option<Vec<Option<NodeId>>>,
}

/// Where an edge leads: a node, or `None` for the end of the conversation.
#[derive(Clone, Copy)]
struct Edge(Option<NodeId>);

impl NodePatch {
    /// The kind the fields imply, or `None` when they imply none.
    const fn kind(&self) -> Result<Option<Kind>, PatchError> {
        let text = self.message.is_some();
        let branch = self.query.is_some() || self.param.is_some() || self.answers.is_some();
        let event = self.event.is_some() || self.params.is_some();
        match (text, branch, event) {
            (false, false, false) => Ok(None),
            (true, false, false) => Ok(Some(Kind::Text)),
            (false, true, false) => Ok(Some(Kind::Branch)),
            (false, false, true) => Ok(Some(Kind::Event)),
            _ => Err(PatchError::MixedNode),
        }
    }

    /// Looks up every reference, handing each that names nothing to `found`.
    /// `None` when any did.
    fn resolve(&self, lookup: &Lookup<'_>, found: &mut impl FnMut(PatchError)) -> Option<Resolved> {
        let mut failed = false;
        let mut keep = |error| {
            failed = true;
            found(error);
        };
        let message = self
            .message
            .as_ref()
            .and_then(|shown| lookup.message(shown).map_err(&mut keep).ok());
        let next = self
            .next
            .as_ref()
            .and_then(|to| lookup.node(to).map(Edge).map_err(&mut keep).ok());
        let answers = self.answers.as_ref().map(|answers| {
            answers
                .iter()
                .filter_map(|to| lookup.node(to).map_err(&mut keep).ok())
                .collect()
        });
        (!failed).then_some(Resolved {
            message,
            next,
            answers,
        })
    }

    /// A node of `kind` with id `id` from these fields alone.
    fn build(&self, id: NodeId, kind: Kind, refs: Resolved) -> Result<Node, PatchError> {
        Ok(match kind {
            Kind::Text => Node::Text {
                id,
                message: refs.message.ok_or(PatchError::WrongField("message"))?,
                next: refs.next.and_then(|Edge(to)| to),
            },
            Kind::Branch => Node::Branch {
                id,
                query: self.query.ok_or(PatchError::WrongField("query"))?,
                param: self.param.unwrap_or(0),
                children: refs.answers.unwrap_or_default(),
            },
            Kind::Event => Node::Event {
                id,
                event: self.event.ok_or(PatchError::WrongField("event"))?,
                params: self.params.unwrap_or_default(),
                next: refs.next.and_then(|Edge(to)| to),
            },
        })
    }

    /// Sets these fields on `node`, or replaces it when they imply another
    /// kind. Leaves `node` alone when any reference names nothing.
    fn patch(
        &self,
        node: &mut Node,
        lookup: &Lookup<'_>,
        found: &mut impl FnMut(PatchError),
    ) -> Result<(), PatchError> {
        let kind = self.kind()?.unwrap_or_else(|| kind_of(node));
        if kind == Kind::Branch && self.next.is_some() {
            return Err(PatchError::WrongField("next"));
        }
        let Some(refs) = self.resolve(lookup, found) else {
            return Ok(());
        };
        if kind != kind_of(node) {
            *node = self.build(node.id(), kind, refs)?;
            return Ok(());
        }
        match node {
            Node::Text { message, next, .. } => {
                *message = refs.message.unwrap_or(*message);
                if let Some(Edge(to)) = refs.next {
                    *next = to;
                }
            }
            Node::Branch {
                query,
                param,
                children,
                ..
            } => {
                *query = self.query.unwrap_or(*query);
                *param = self.param.unwrap_or(*param);
                if let Some(answers) = refs.answers {
                    *children = answers;
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
                if let Some(Edge(to)) = refs.next {
                    *next = to;
                }
            }
        }
        Ok(())
    }

    /// A new node with id `id` from these fields, or `None` when any
    /// reference names nothing.
    fn create(
        &self,
        id: NodeId,
        lookup: &Lookup<'_>,
        found: &mut impl FnMut(PatchError),
    ) -> Result<Option<Node>, PatchError> {
        let kind = self.kind()?.ok_or(PatchError::NoKind)?;
        if kind == Kind::Branch && self.next.is_some() {
            return Err(PatchError::WrongField("next"));
        }
        self.resolve(lookup, found)
            .map(|refs| self.build(id, kind, refs))
            .transpose()
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

fn apply_flow(
    bmg: &mut Bmg,
    patch: &BmgPatch,
    lookup: &Lookup<'_>,
    found: &mut Vec<PatchDiagnostic>,
) {
    let touches = !patch.node.is_empty()
        || !patch.root.is_empty()
        || !patch.new.node.is_empty()
        || patch.new.flow
        || !patch.remove.nodes.is_empty()
        || !patch.remove.roots.is_empty();
    if patch.remove.flow {
        if touches {
            found.push(Diagnostic::error(PatchError::RemovedFlow, Entry::Flow));
        } else if bmg.flow.take().is_none() {
            found.push(Diagnostic::error(PatchError::NoFlow, Entry::Flow));
        }
        return;
    }
    if patch.new.flow {
        if bmg.flow.is_some() {
            found.push(Diagnostic::error(PatchError::HasFlow, Entry::Flow));
        } else {
            bmg.flow = Some(Flow::default());
        }
    }
    if !touches {
        return;
    }
    let Some(flow) = bmg.flow.as_mut() else {
        found.push(Diagnostic::error(PatchError::NoFlow, Entry::Flow));
        return;
    };

    for (Number(at), change) in &patch.node {
        let entry = Entry::Node(*at);
        let mut keep = |error| found.push(Diagnostic::error(error, entry.clone()));
        let patched = flow
            .nodes
            .iter_mut()
            .find(|node| node.id() == NodeId(*at))
            .ok_or(PatchError::UnknownNode(*at))
            .and_then(|node| change.patch(node, lookup, &mut keep));
        if let Err(error) = patched {
            keep(error);
        }
    }
    for (id, new) in (lookup.ids.nodes..).zip(&patch.new.node) {
        let entry = Entry::NewNode(new.name.clone());
        let mut keep = |error| found.push(Diagnostic::error(error, entry.clone()));
        match NodePatch::from(new).create(NodeId(id), lookup, &mut keep) {
            Ok(node) => flow.nodes.extend(node),
            Err(error) => keep(error),
        }
    }
    for at in &patch.remove.nodes {
        let before = flow.nodes.len();
        flow.nodes.retain(|node| node.id() != NodeId(*at));
        if flow.nodes.len() == before {
            found.push(Diagnostic::error(
                PatchError::UnknownNode(*at),
                Entry::Remove,
            ));
        }
    }

    apply_roots(flow, patch, lookup, found);
}

/// Sets and removes the roots `patch` names.
fn apply_roots(
    flow: &mut Flow,
    patch: &BmgPatch,
    lookup: &Lookup<'_>,
    found: &mut Vec<PatchDiagnostic>,
) {
    for (Number(public_id), to) in &patch.root {
        let node = match lookup.node(to) {
            Ok(Some(node)) => node,
            Ok(None) => {
                found.push(Diagnostic::error(
                    PatchError::EndRoot,
                    Entry::Root(*public_id),
                ));
                continue;
            }
            Err(error) => {
                found.push(Diagnostic::error(error, Entry::Root(*public_id)));
                continue;
            }
        };
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
        match flow
            .roots
            .iter()
            .position(|root| root.public_id == *public_id)
        {
            Some(at) => {
                flow.roots.remove(at);
            }
            None => found.push(Diagnostic::error(
                PatchError::UnknownRoot(*public_id),
                Entry::Remove,
            )),
        }
    }
}

/// Every message a node shows and every node an edge or root reaches must
/// still be there.
fn check_graph(bmg: &Bmg, names: &Names, found: &mut Vec<PatchDiagnostic>) {
    let Some(flow) = &bmg.flow else {
        return;
    };
    let entry = |id: NodeId| {
        names
            .nodes
            .get(&id)
            .map_or(Entry::Node(id.0), |name| Entry::NewNode(name.clone()))
    };
    let describe = |id: NodeId| {
        names
            .nodes
            .get(&id)
            .map_or_else(|| NodeRef::Vanilla(id.0).to_string(), Clone::clone)
    };
    let messages: HashSet<MessageId> = bmg.messages.iter().map(|message| message.id).collect();
    let nodes: HashSet<NodeId> = flow.nodes.iter().map(Node::id).collect();
    for node in &flow.nodes {
        let (shown, next, children) = match node {
            Node::Text { message, next, .. } => (Some(*message), *next, &[][..]),
            Node::Branch { children, .. } => (None, None, children.as_slice()),
            Node::Event { next, .. } => (None, *next, &[][..]),
        };
        if shown.is_some_and(|message| !messages.contains(&message)) {
            found.push(Diagnostic::error(
                PatchError::RemovedMessage,
                entry(node.id()),
            ));
        }
        for to in next.iter().chain(children.iter().flatten()) {
            if !nodes.contains(to) {
                found.push(Diagnostic::error(
                    PatchError::RemovedNode(describe(*to)),
                    entry(node.id()),
                ));
            }
        }
    }
    for root in &flow.roots {
        if !nodes.contains(&root.node) {
            found.push(Diagnostic::error(
                PatchError::RemovedNode(describe(root.node)),
                Entry::Root(root.public_id),
            ));
        }
    }
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
