//! What the game's tables find odd in a message file that the file can still
//! hold. Nothing here is an error: the game reads all of it, just maybe not
//! the way the modder meant.
//!
//! Every check is a lookup in `tpmt_tables`, so a row added to a table is
//! checked against with no change here.
//!
//! The tables don't name every value retail files use, so a vanilla file can
//! turn some of these up. [`BmgSession`](super::BmgSession) only checks what
//! differs from vanilla.

use tpmt_message::{Message, MessageId, Node, NodeId, TextSegment};
use tpmt_report::Diagnostic;
use tpmt_tables::message::flow::{EVENTS, QUERIES};
use tpmt_tables::message::tag;
use tpmt_tables::{Edition, find};

use super::EditableBmg;
use super::tables::{Tables, read_field};

/// Something [`EditableBmg::check`] found, and where.
pub type BmgDiagnostic = Diagnostic<Issue, At>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Issue {
    #[error("no tag is group {group}, code {code} in this version")]
    UnknownTag { group: u8, code: u16 },

    #[error("`{tag}` doesn't take {actual} argument bytes")]
    TagArgs { tag: &'static str, actual: usize },

    /// `name` is the tag's or the record field's.
    #[error("{value} is not a value the game names for `{name}`")]
    UnnamedValue { name: &'static str, value: u8 },

    #[error("no branch query is {0} in this version")]
    UnknownQuery(u16),

    #[error("no event is {0} in this version")]
    UnknownEvent(u8),

    #[error("no layout is {0} bytes wide, so record fields can only be set as raw attributes")]
    Unlaid(u16),
}

/// Where in a message file an [`Issue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum At {
    /// The file as a whole.
    File,
    Message {
        id: MessageId,
        property: Property,
    },
    Node(NodeId),
}

/// The part of a message an [`Issue`] is in, as an inspector lays it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Property {
    Text,
    /// A record field, by its layout name.
    Field(&'static str),
}

/// A message or node, for picking which ones [`EditableBmg::check`] looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Message(MessageId),
    Node(NodeId),
}

impl EditableBmg {
    /// What the tables find odd in the file as a whole, and in each message
    /// and node `wanted` picks.
    pub fn check(&self, wanted: impl Fn(Item) -> bool) -> Vec<BmgDiagnostic> {
        let tables = self.tables();
        let bmg = self.bmg();
        let mut found = Vec::new();
        // If the INF1 record width doesn't match any layout for the file's edition
        if tables.layout.is_none() {
            found.push(Diagnostic::info(Issue::Unlaid(bmg.record_len), At::File));
        }

        for message in &bmg.messages {
            if wanted(Item::Message(message.id)) {
                check_message(tables, message, &mut found);
            }
        }

        let nodes = bmg.flow.iter().flat_map(|flow| &flow.nodes);
        for node in nodes.filter(|node| wanted(Item::Node(node.id()))) {
            let issue = match *node {
                Node::Branch { query, .. } => find(QUERIES, query, tables.edition)
                    .is_none()
                    .then_some(Issue::UnknownQuery(query)),
                Node::Event { event, .. } => find(EVENTS, event, tables.edition)
                    .is_none()
                    .then_some(Issue::UnknownEvent(event)),
                // has nothing for the game's tables to look up
                Node::Text { .. } => None,
            };
            found.extend(issue.map(|issue| Diagnostic::warning(issue, At::Node(node.id()))));
        }
        found
    }
}

fn check_message(tables: &Tables, message: &Message, found: &mut Vec<BmgDiagnostic>) {
    let at = |property| At::Message {
        id: message.id,
        property,
    };

    for segment in &message.text {
        if let TextSegment::Tag { group, code, args } = segment
            && let Some(issue) = tag_issue(*group, *code, args, tables.edition)
        {
            found.push(Diagnostic::warning(issue, at(Property::Text)));
        }
    }

    if !tables.named(&message.attributes) {
        return;
    }
    for field in tables.fields() {
        let Some(values) = field.values else {
            continue;
        };
        let value =
            read_field(&message.attributes, field).and_then(|value| u8::try_from(value).ok());
        if let Some(value) = value
            && find(values, value, tables.edition).is_none()
        {
            found.push(Diagnostic::warning(
                Issue::UnnamedValue {
                    name: field.name,
                    value,
                },
                at(Property::Field(field.name)),
            ));
        }
    }
}

fn tag_issue(group: u8, code: u16, args: &[u8], edition: Edition) -> Option<Issue> {
    let Some(tag) = tag::find(group, code, edition) else {
        return Some(Issue::UnknownTag { group, code });
    };
    // A varying length is a u8 then the rest, so at least 1.
    let fits = tag
        .args
        .fixed_len()
        .map_or(!args.is_empty(), |len| args.len() == len);
    if !fits {
        return Some(Issue::TagArgs {
            tag: tag.name,
            actual: args.len(),
        });
    }
    match (tag.values, args) {
        (Some(values), [value]) if find(values, *value, edition).is_none() => {
            Some(Issue::UnnamedValue {
                name: tag.name,
                value: *value,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use tpmt_binary::Format;
    use tpmt_message::{Bmg, Encoding, Flow, Mid1Header};
    use tpmt_tables::Version;
    use tpmt_tables::message::record;

    use super::*;
    use crate::message::{BmgSession, MessageChange, MessageEdit};

    const EDITION: Edition = Edition::default_language(Version::GcnUsa);

    fn tag(group: u8, code: u16, args: &[u8]) -> TextSegment {
        TextSegment::Tag {
            group,
            code,
            args: args.into(),
        }
    }

    /// One message with an odd tag of each kind and an unnamed box kind, and
    /// a branch and an event the tables don't know.
    fn odd() -> Bmg {
        let mut attributes = [0; 16];
        attributes[5] = 200;
        Bmg {
            encoding: Encoding::ShiftJis,
            record_len: record::LAYOUT.len,
            mid1: Some(Mid1Header::default()),
            messages: vec![Message {
                public_id: 0,
                id: MessageId(0),
                attributes: Box::new(attributes),
                text: vec![tag(7, 3, &[]), tag(0, 7, &[30]), tag(255, 0, &[200])],
            }],
            flow: Some(Flow {
                nodes: vec![
                    Node::Branch {
                        id: NodeId(0),
                        query: 9999,
                        param: 0,
                        children: Vec::new(),
                    },
                    Node::Event {
                        id: NodeId(1),
                        event: 255,
                        params: [0; 4],
                        next: None,
                    },
                ],
                roots: Vec::new(),
            }),
            strings: None,
        }
    }

    #[test]
    fn every_lookup_that_misses_is_found() {
        let file = EditableBmg::new(odd(), EDITION);
        let text = At::Message {
            id: MessageId(0),
            property: Property::Text,
        };
        let box_kind = At::Message {
            id: MessageId(0),
            property: Property::Field("Box kind"),
        };
        assert_eq!(
            file.check(|_| true),
            [
                Diagnostic::warning(Issue::UnknownTag { group: 7, code: 3 }, text),
                Diagnostic::warning(
                    Issue::TagArgs {
                        tag: "Pause",
                        actual: 1
                    },
                    text
                ),
                Diagnostic::warning(
                    Issue::UnnamedValue {
                        name: "Color",
                        value: 200
                    },
                    text
                ),
                Diagnostic::warning(
                    Issue::UnnamedValue {
                        name: "Box kind",
                        value: 200
                    },
                    box_kind
                ),
                Diagnostic::warning(Issue::UnknownQuery(9999), At::Node(NodeId(0))),
                Diagnostic::warning(Issue::UnknownEvent(255), At::Node(NodeId(1))),
            ]
        );
        assert_eq!(file.check(|_| false), []);
    }

    #[test]
    fn records_no_layout_names_are_a_note() {
        let bmg = Bmg {
            record_len: 7,
            messages: Vec::new(),
            ..odd()
        };
        assert_eq!(
            EditableBmg::new(bmg, EDITION).check(|_| false),
            [Diagnostic::info(Issue::Unlaid(7), At::File)]
        );
    }

    /// Vanilla's own oddities are the tables' gaps, not the modder's, so a
    /// session only checks what it changed.
    #[test]
    fn a_session_checks_only_what_differs_from_vanilla() {
        let mut vanilla = odd();
        vanilla.flow = None;
        vanilla.messages.push(Message {
            public_id: 1,
            id: MessageId(1),
            attributes: Box::new([0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            text: Vec::new(),
        });
        let mut session = BmgSession::patched(&vanilla.encode().unwrap(), None, EDITION).unwrap();
        assert_eq!(session.issues(), []);

        session
            .apply(MessageEdit::Set {
                id: MessageId(1),
                change: MessageChange::Text(vec![tag(7, 3, &[])]),
            })
            .unwrap();
        assert_eq!(
            session.issues(),
            [Diagnostic::warning(
                Issue::UnknownTag { group: 7, code: 3 },
                At::Message {
                    id: MessageId(1),
                    property: Property::Text
                }
            )]
        );
        assert_eq!(
            session.reports("zel_00.bmg")[0].to_string(),
            "`zel_00.bmg`, message 1 text: no tag is group 7, code 3 in this version"
        );

        session.undo().unwrap();
        assert_eq!(session.issues(), []);
    }
}
