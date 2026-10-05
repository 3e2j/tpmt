//! An edited file to the patch that makes it from vanilla.

use std::collections::{HashMap, HashSet};

use tpmt_message::{Bmg, Flow, Message, MessageId, Node, NodeId};
use tpmt_tables::Edition;

use super::ids::Ids;
use super::{
    BmgPatch, MessagePatch, MessageRef, Names, NewMessage, NewNode, NodePatch, NodeRef, Number,
};
use crate::Status;
use crate::message::BmgChanges;
use crate::message::tables::text::hex;
use crate::message::tables::{Tables, read_field};

/// What makes `edited` from `vanilla`.
///
/// `vanilla` must be a fresh decode of the file, so its ids are its
/// positions, and `edited` must have come from it through
/// [`apply`](fn@super::apply) and edits, so a vanilla item kept its id and a
/// new one got an id past every vanilla one. New items without a name in
/// `names` get one there.
#[must_use]
pub fn diff(vanilla: &Bmg, edited: &Bmg, edition: Edition, names: &mut Names) -> BmgPatch {
    let tables = Tables::new(vanilla, edition);
    let ids = Ids::new(vanilla);
    name_new(&ids, edited, names);
    let refs = Refs { ids: &ids, names };

    let changes = BmgChanges::new(Some(vanilla), edited);
    let mut patch = BmgPatch::default();
    let edited_messages: HashMap<MessageId, &Message> = edited
        .messages
        .iter()
        .map(|message| (message.id, message))
        .collect();
    let (kept, added) = changes.messages.split_at(vanilla.messages.len());
    for ((message, key), (id, status)) in vanilla.messages.iter().zip(&ids.keys).zip(kept) {
        match status {
            Status::Changed => {
                let entry = message_patch(&tables, message, edited_messages[id]);
                if entry != MessagePatch::default() {
                    patch.message.insert(*key, entry);
                }
            }
            Status::Removed => patch.remove.messages.push(*key),
            Status::Unchanged | Status::Added => {}
        }
    }
    for (id, _) in added {
        patch.new.message.push(new_message(
            &tables,
            edited_messages[id],
            &refs.names.messages[id],
        ));
    }

    match (&vanilla.flow, &edited.flow) {
        (Some(_), None) => patch.remove.flow = true,
        (vanilla_flow, Some(flow)) => {
            patch.new.flow = vanilla_flow.is_none();
            diff_flow(vanilla_flow.as_ref(), flow, &changes, &refs, &mut patch);
        }
        (None, None) => {}
    }
    patch
}

/// What changed from `vanilla` to `edited`.
fn message_patch(tables: &Tables, vanilla: &Message, edited: &Message) -> MessagePatch {
    let mut patch = MessagePatch::default();
    if edited.text != vanilla.text {
        patch.text = Some(tables.render(&edited.text));
    }
    if tables.has_mid1 && edited.public_id != vanilla.public_id {
        patch.public_id = Some(edited.public_id);
    }
    if tables.named(&vanilla.attributes) && tables.named(&edited.attributes) {
        for field in tables.fields() {
            let value = read_field(&edited.attributes, field);
            if let Some(value) =
                value.filter(|value| Some(*value) != read_field(&vanilla.attributes, field))
            {
                patch.fields.insert(field.name.to_string(), value);
            }
        }
    } else if edited.attributes != vanilla.attributes {
        patch.attributes = Some(hex(&edited.attributes));
    }
    patch
}

/// `message` whole, as a patch adds it.
fn new_message(tables: &Tables, message: &Message, name: &str) -> NewMessage {
    let mut new = NewMessage {
        name: name.to_string(),
        public_id: tables.has_mid1.then_some(message.public_id),
        text: tables.render(&message.text),
        ..NewMessage::default()
    };
    if tables.named(&message.attributes) {
        for field in tables.fields() {
            if let Some(value) = read_field(&message.attributes, field).filter(|value| *value != 0)
            {
                new.fields.insert(field.name.to_string(), value);
            }
        }
    } else if message.attributes.iter().any(|byte| *byte != 0) {
        new.attributes = Some(hex(&message.attributes));
    }
    new
}

/// Turns ids in the edited file into what a patch calls them.
struct Refs<'a> {
    ids: &'a Ids,
    names: &'a Names,
}

impl Refs<'_> {
    fn message(&self, id: MessageId) -> MessageRef {
        let vanilla = usize::try_from(id.0)
            .ok()
            .and_then(|at| self.ids.keys.get(at));
        match vanilla {
            Some(key) if id.0 < self.ids.messages => MessageRef::Vanilla(*key),
            _ => MessageRef::New(self.names.messages[&id].clone()),
        }
    }

    fn node(&self, id: Option<NodeId>) -> NodeRef {
        match id {
            None => NodeRef::End,
            Some(id) if id.0 < self.ids.nodes => NodeRef::Vanilla(id.0),
            Some(id) => NodeRef::New(self.names.nodes[&id].clone()),
        }
    }

    /// Every field of `node`, for a new node or one whose kind changed.
    fn whole(&self, node: &Node) -> NodePatch {
        match node {
            Node::Text { message, next, .. } => NodePatch {
                message: Some(self.message(*message)),
                next: Some(self.node(*next)),
                ..NodePatch::default()
            },
            Node::Branch {
                query,
                param,
                children,
                ..
            } => NodePatch {
                query: Some(*query),
                param: Some(*param),
                answers: Some(children.iter().map(|child| self.node(*child)).collect()),
                ..NodePatch::default()
            },
            Node::Event {
                event,
                params,
                next,
                ..
            } => NodePatch {
                event: Some(*event),
                params: Some(*params),
                next: Some(self.node(*next)),
                ..NodePatch::default()
            },
        }
    }

    fn node_patch(&self, vanilla: &Node, edited: &Node) -> NodePatch {
        match (vanilla, edited) {
            (
                Node::Text { message, next, .. },
                Node::Text {
                    message: new_message,
                    next: new_next,
                    ..
                },
            ) => NodePatch {
                message: changed(message, new_message).map(|id| self.message(*id)),
                next: changed(next, new_next).map(|next| self.node(*next)),
                ..NodePatch::default()
            },
            (
                Node::Branch {
                    query,
                    param,
                    children,
                    ..
                },
                Node::Branch {
                    query: new_query,
                    param: new_param,
                    children: new_children,
                    ..
                },
            ) => NodePatch {
                query: changed(query, new_query).copied(),
                param: changed(param, new_param).copied(),
                answers: changed(children, new_children)
                    .map(|children| children.iter().map(|child| self.node(*child)).collect()),
                ..NodePatch::default()
            },
            (
                Node::Event {
                    event,
                    params,
                    next,
                    ..
                },
                Node::Event {
                    event: new_event,
                    params: new_params,
                    next: new_next,
                    ..
                },
            ) => NodePatch {
                event: changed(event, new_event).copied(),
                params: changed(params, new_params).copied(),
                next: changed(next, new_next).map(|next| self.node(*next)),
                ..NodePatch::default()
            },
            _ => self.whole(edited),
        }
    }
}

/// `new`, when it isn't `old`.
fn changed<'a, T: PartialEq>(old: &T, new: &'a T) -> Option<&'a T> {
    (old != new).then_some(new)
}

/// Names every new item in `edited` that has none yet, and forgets names of
/// items that are gone.
fn name_new(ids: &Ids, edited: &Bmg, names: &mut Names) {
    let messages: Vec<MessageId> = edited
        .messages
        .iter()
        .map(|message| message.id)
        .filter(|id| id.0 >= ids.messages)
        .collect();
    let nodes: Vec<NodeId> = edited
        .flow
        .iter()
        .flat_map(|flow| &flow.nodes)
        .map(Node::id)
        .filter(|id| id.0 >= ids.nodes)
        .collect();
    names.messages.retain(|id, _| messages.contains(id));
    names.nodes.retain(|id, _| nodes.contains(id));

    let mut taken: HashSet<String> = names
        .messages
        .values()
        .chain(names.nodes.values())
        .cloned()
        .collect();
    let mut fresh = |prefix: &str| {
        let name = (1..=u32::MAX)
            .map(|n| format!("{prefix}_{n}"))
            .find(|name| !taken.contains(name))
            .unwrap_or_default();
        taken.insert(name.clone());
        name
    };
    for id in messages {
        names.messages.entry(id).or_insert_with(|| fresh("message"));
    }
    for id in nodes {
        names.nodes.entry(id).or_insert_with(|| fresh("node"));
    }
}

fn diff_flow(
    vanilla: Option<&Flow>,
    edited: &Flow,
    changes: &BmgChanges,
    refs: &Refs<'_>,
    patch: &mut BmgPatch,
) {
    let edited_nodes: HashMap<NodeId, &Node> =
        edited.nodes.iter().map(|node| (node.id(), node)).collect();
    let vanilla_nodes = vanilla.map_or(&[][..], |flow| &flow.nodes);
    let (kept, added) = changes.nodes.split_at(vanilla_nodes.len());
    for (node, (id, status)) in vanilla_nodes.iter().zip(kept) {
        match status {
            Status::Changed => {
                let entry = refs.node_patch(node, edited_nodes[id]);
                if entry != NodePatch::default() {
                    patch.node.insert(Number(id.0), entry);
                }
            }
            Status::Removed => patch.remove.nodes.push(id.0),
            Status::Unchanged | Status::Added => {}
        }
    }
    for (id, _) in added {
        let whole = refs.whole(edited_nodes[id]);
        patch.new.node.push(NewNode {
            name: refs.names.nodes[id].clone(),
            message: whole.message,
            query: whole.query,
            param: whole.param,
            answers: whole.answers,
            event: whole.event,
            params: whole.params,
            next: whole.next,
        });
    }

    let mut edited_roots = HashMap::with_capacity(edited.roots.len());
    for root in &edited.roots {
        edited_roots.entry(root.public_id).or_insert(root.node);
    }
    for (public_id, status) in &changes.roots {
        match status {
            Status::Changed | Status::Added => {
                let node = edited_roots[public_id];
                patch.root.insert(Number(*public_id), refs.node(Some(node)));
            }
            Status::Removed => patch.remove.roots.push(*public_id),
            Status::Unchanged => {}
        }
    }
}
