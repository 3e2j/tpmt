use tpmt_message::{
    Bmg, Encoding, Flow, Message, MessageId, Mid1Header, Node, NodeId, Root, TextSegment,
};
use tpmt_report::Diagnostic;
use tpmt_tables::message::{Field, record};
use tpmt_tables::{Edition, Version};

use super::*;
use crate::message::{
    BmgEdit, EditableBmg, FlowEdit, ListEdit, MessageChange, MessageEdit, NodeChange, NodeEdit,
};
use crate::{Editable, History};

const EDITION: Edition = Edition::default_language(Version::GcnUsa);

fn message(id: u32, public_id: u16, text: &[u8]) -> Message {
    let mut attributes = [0; 16];
    attributes[..2].copy_from_slice(&public_id.to_be_bytes());
    Message {
        public_id,
        id: MessageId(id),
        attributes: Box::new(attributes),
        text: vec![TextSegment::Text(text.into())],
    }
}

/// Three messages, and a flow of two text nodes under one root.
fn vanilla() -> Bmg {
    Bmg {
        encoding: Encoding::ShiftJis,
        record_len: record::LAYOUT.len,
        mid1: Some(Mid1Header::default()),
        messages: vec![
            message(0, 100, b"Hello"),
            message(1, 101, b"Bye"),
            message(2, 102, b"Unused"),
        ],
        flow: Some(Flow {
            nodes: vec![
                Node::Text {
                    id: NodeId(0),
                    message: MessageId(0),
                    next: Some(NodeId(1)),
                },
                Node::Text {
                    id: NodeId(1),
                    message: MessageId(1),
                    next: None,
                },
            ],
            roots: vec![Root {
                public_id: 300,
                node: NodeId(0),
            }],
        }),
        strings: None,
    }
}

fn box_kind() -> Field {
    record::LAYOUT.fields[3]
}

/// `edits` on the vanilla file, through a file as an editor would.
fn edited(edits: impl FnOnce(&mut EditableBmg) -> Vec<BmgEdit>) -> Bmg {
    let mut file = EditableBmg::new(vanilla(), EDITION);
    let mut history = History::default();
    for edit in edits(&mut file) {
        history.apply(&mut file, edit).unwrap();
    }
    file.bmg().clone()
}

/// Diffs `edited`, sends the patch through TOML, and applies it.
fn round_trip(edited: &Bmg) -> (String, Bmg) {
    let mut names = Names::default();
    let patch = diff(&vanilla(), edited, EDITION, &mut names);
    let toml = patch.to_toml().unwrap();
    let read = BmgPatch::from_toml(&toml).unwrap();
    assert_eq!(read, patch);
    let (applied, applied_names) = apply(&vanilla(), &read, EDITION).unwrap();
    assert_eq!(applied_names, names);
    (toml, applied)
}

#[test]
fn an_unedited_file_has_an_empty_patch() {
    let patch = diff(&vanilla(), &vanilla(), EDITION, &mut Names::default());
    assert!(patch.is_empty());
    assert_eq!(patch.to_toml().unwrap(), "");
    assert_eq!(apply(&vanilla(), &patch, EDITION).unwrap().0, vanilla());
}

#[test]
fn a_patch_holds_only_what_changed() {
    let bmg = edited(|_| {
        vec![
            MessageEdit::Set {
                id: MessageId(1),
                change: MessageChange::Text(vec![
                    TextSegment::Text(b"See you,\n".as_slice().into()),
                    TextSegment::Tag {
                        group: 0,
                        code: 0,
                        args: Box::default(),
                    },
                ]),
            }
            .into(),
            MessageEdit::Set {
                id: MessageId(0),
                change: MessageChange::Field {
                    field: box_kind(),
                    value: 13,
                },
            }
            .into(),
            MessageEdit::Set {
                id: MessageId(0),
                change: MessageChange::PublicId(150),
            }
            .into(),
            MessageEdit::Remove(MessageId(2)).into(),
        ]
    });
    let (toml, applied) = round_trip(&bmg);
    assert_eq!(applied, bmg);
    assert_eq!(
        toml,
        r#"[message.100]
public_id = 150

[message.100.fields]
"Box kind" = 13

[message.101]
text = """
See you,
{Player name}"""

[remove]
messages = ["102"]
"#
    );
}

#[test]
fn new_messages_and_nodes_are_named_and_wired() {
    let bmg = edited(|file| {
        let message = file.unused_message_id();
        let node = file.unused_node_id();
        vec![
            MessageEdit::Insert {
                at: 3,
                message: Message {
                    public_id: 9000,
                    ..message_with(message, b"New")
                },
            }
            .into(),
            NodeEdit::Insert {
                at: 2,
                node: Node::Text {
                    id: node,
                    message,
                    next: Some(NodeId(1)),
                },
            }
            .into(),
            NodeEdit::Set {
                id: NodeId(0),
                change: NodeChange::Next(Some(node)),
            }
            .into(),
            FlowEdit::Root(ListEdit::Insert {
                at: 1,
                value: Root {
                    public_id: 301,
                    node,
                },
            })
            .into(),
        ]
    });
    let (toml, applied) = round_trip(&bmg);
    assert_eq!(applied, bmg);
    assert_eq!(
        toml,
        r#"[node.0]
next = "node_1"

[root]
301 = "node_1"

[[new.message]]
name = "message_1"
public_id = 9000
text = "New"

[[new.node]]
name = "node_1"
message = "message_1"
next = "node:1"
"#
    );
}

fn message_with(id: MessageId, text: &[u8]) -> Message {
    Message {
        public_id: 0,
        id,
        attributes: Box::new([0; 16]),
        text: vec![TextSegment::Text(text.into())],
    }
}

/// A node replaced by one of another kind under the same id is stored
/// whole.
#[test]
fn a_node_that_changes_kind_is_stored_whole() {
    let mut bmg = vanilla();
    bmg.flow.as_mut().unwrap().nodes[1] = Node::Event {
        id: NodeId(1),
        event: 4,
        params: [0, 0, 0, 7],
        next: None,
    };
    let (toml, applied) = round_trip(&bmg);
    assert_eq!(applied, bmg);
    assert_eq!(
        toml,
        "[node.1]\nevent = 4\nparams = [0, 0, 0, 7]\nnext = \"end\"\n"
    );
}

#[test]
fn an_edit_put_back_drops_out() {
    let bmg = edited(|_| {
        let set = |text: &[u8]| -> BmgEdit {
            MessageEdit::Set {
                id: MessageId(0),
                change: MessageChange::Text(vec![TextSegment::Text(text.into())]),
            }
            .into()
        };
        vec![set(b"Changed"), set(b"Hello")]
    });
    assert!(diff(&vanilla(), &bmg, EDITION, &mut Names::default()).is_empty());
}

#[test]
fn saving_twice_writes_the_same_patch() {
    let bmg = edited(|file| {
        vec![
            MessageEdit::Insert {
                at: 0,
                message: message_with(file.unused_message_id(), b"A"),
            }
            .into(),
        ]
    });
    let mut names = Names::default();
    let first = diff(&vanilla(), &bmg, EDITION, &mut names)
        .to_toml()
        .unwrap();
    let second = diff(&vanilla(), &bmg, EDITION, &mut names)
        .to_toml()
        .unwrap();
    assert_eq!(first, second);
}

/// Removing the last vanilla node frees its id, and a new node must not
/// take it, or the patch would read it as the vanilla one.
#[test]
fn a_new_node_never_takes_a_vanilla_id() {
    let mut file = EditableBmg::new(vanilla(), EDITION);
    file.perform(
        NodeEdit::Set {
            id: NodeId(0),
            change: NodeChange::Next(None),
        }
        .into(),
    )
    .unwrap();
    file.perform(NodeEdit::Remove(NodeId(1)).into()).unwrap();
    assert_eq!(file.unused_node_id(), NodeId(2));
}

#[test]
fn a_removed_message_still_shown_is_refused() {
    let patch = BmgPatch {
        remove: Remove {
            messages: vec![MessageKey::Id(101)],
            ..Remove::default()
        },
        ..BmgPatch::default()
    };
    assert_eq!(
        apply(&vanilla(), &patch, EDITION),
        Err(vec![Diagnostic::error(
            PatchError::RemovedMessage,
            Entry::Node(1)
        )])
    );
}

#[test]
fn a_message_goes_by_position_too() {
    let patch = BmgPatch::from_toml("[message.\"@2\"]\ntext = \"Used\"\n").unwrap();
    let (bmg, _) = apply(&vanilla(), &patch, EDITION).unwrap();
    assert_eq!(
        bmg.messages[2].text,
        [TextSegment::Text(b"Used".as_slice().into())]
    );
}

#[test]
fn bad_references_are_refused() {
    let apply = |toml: &str| apply(&vanilla(), &BmgPatch::from_toml(toml).unwrap(), EDITION);
    let one = |error, entry| Err(vec![Diagnostic::error(error, entry)]);
    assert_eq!(
        apply("[message.999]\ntext = \"x\"\n"),
        one(
            PatchError::UnknownMessage(MessageKey::Id(999)),
            Entry::Message(MessageKey::Id(999))
        )
    );
    assert_eq!(
        apply("[node.0]\nnext = \"nowhere\"\n"),
        one(
            PatchError::UnknownName("nowhere".to_string()),
            Entry::Node(0)
        )
    );
    assert_eq!(
        apply("[message.100.fields]\n\"Message id\" = 3\n"),
        one(
            PatchError::IdField("Message id"),
            Entry::Message(MessageKey::Id(100))
        )
    );
    assert_eq!(
        apply("[[new.message]]\nname = \"9lives\"\ntext = \"\"\n"),
        one(
            PatchError::BadName("9lives".to_string()),
            Entry::NewMessage("9lives".to_string())
        )
    );
    assert!(BmgPatch::from_toml("[message.100]\ntxet = \"x\"\n").is_err());
}

/// One bad entry doesn't hide the next, and a message's problems are all
/// named, not only its first.
#[test]
fn every_bad_entry_is_named() {
    let patch = BmgPatch::from_toml(
        "[message.100]\ntext = \"{No tag}{Pause:x y}\"\n\n\
         [message.101.fields]\nNope = 1\n\n\
         [node.0]\nnext = \"nowhere\"\n\n\
         [remove]\nroots = [7]\n",
    )
    .unwrap();
    let found = apply(&vanilla(), &patch, EDITION).unwrap_err();
    let at: Vec<_> = found
        .iter()
        .map(|found| (found.at.to_string(), found.code.to_string()))
        .collect();
    assert_eq!(
        at,
        [
            ("[message.100]".into(), "`No tag` is not a tag".into()),
            (
                "[message.100]".into(),
                "`x y` is not a valid argument for Pause".into()
            ),
            (
                "[message.101]".into(),
                "the layout has no field `Nope`".into()
            ),
            ("[node.0]".into(), "nothing new is named `nowhere`".into()),
            ("[remove]".into(), "the flow has no root 7".into()),
        ] as [(String, String); 5]
    );
}
