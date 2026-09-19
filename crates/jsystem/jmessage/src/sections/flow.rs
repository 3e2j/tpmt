//! The flow graph: FLW1 and FLI1.
//!
//! A message on its own is one box of text. A flow is what turns a pile of
//! them into a conversation: nodes that display a message, branch on something
//! the game knows, or fire an event, each pointing at what comes next.
//!
//! # Layout
//!
//! ```text
//! FLW1
//!   header    how many node records, how many indirection table entries
//!   nodes     the node table: one 8 byte record per node
//!   table     the indirection table: u16 node positions, for the edges a
//!             record has no room for
//!   mask      one byte per indirection table entry, 0xFF for a dead end
//! FLI1
//!   header    how many roots, and the entry width
//!   roots     one 8 byte entry per root: a flow id and the node it enters at
//! ```
//!
//! The indirection table holds the edges that don't fit in a record: a
//! branch owns one entry per answer, an event owns exactly one (its next
//! pointer, spelled the long way round). A text node's next pointer stays in
//! the record. A dead end is marked 0xFFFF wherever the edge is stored.
//!
//! The mask is a representation of where dead ends occur in the indirection
//! table: the game computes a pointer to but never dereferences. We check it
//! against the table on read and write it nonetheless.
//!
//! Each array inside FLW1 is padded to a multiple of 16 bytes, and the header
//! stores the padded counts. Section alignment only pads the tail.

use std::collections::HashMap;

use tpmt_bytes::{Reader, Writer};

use crate::sections::positions;
use crate::{Error, MessageId, Result};

/// Stable handle for a node, held by whatever points at one.
///
/// Assigned at unpack, where it is the node's own position, and meaningless
/// after that. Edges are these rather than positions, so inserting or removing
/// a node cannot silently repoint an edge at whatever slid into its place.
/// Positions are worked out fresh when the graph is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// One node of the graph.
///
/// Which of the three a record is comes out of its first byte. A record whose
/// type is none of them is padding, of which there is at most one, at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Displays a message, then carries on.
    Text {
        id: NodeId,
        message: MessageId,
        /// `None` is the end of the conversation, stored as an edge to nowhere.
        next: Option<NodeId>,
    },
    /// Asks the game something and takes one of several ways out.
    Branch {
        id: NodeId,
        /// Which question, by its index in the game's own table of them.
        query: u16,
        /// The one value the question is asked about.
        param: u16,
        /// Where each answer goes, in the order the query numbers them. How
        /// many there are is stored, but it is only ever this many, so it is
        /// worked out again when the graph is written.
        children: Vec<Option<NodeId>>,
    },
    /// Makes something happen: an item given, a flag set, an effect played.
    Event {
        id: NodeId,
        /// Which one, by its index in the game's own table of them.
        event: u8,
        /// The four bytes the event reads its arguments out of. How they are
        /// split up is per event and is game data, so they stay raw here.
        params: [u8; 4],
        next: Option<NodeId>,
    },
}

impl Node {
    pub const fn id(&self) -> NodeId {
        match self {
            Self::Text { id, .. } | Self::Branch { id, .. } | Self::Event { id, .. } => *id,
        }
    }
}

/// One way into the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    /// The id external callers ask the game for.
    ///
    /// An id at or above 3000 is redirected to whichever flow resource the
    /// current stage brought with it, so a root numbered that high resolves
    /// against a different file at runtime than the one it is stored in. That
    /// is the game's own convention rather than anything the format says, and
    /// this crate stores what it is given either way.
    pub public_id: u16,
    /// The node the id starts the graph at.
    ///
    /// A node no root names and no edge reaches is dead but still stored.
    pub node: NodeId,
}

/// A whole flow graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flow {
    /// Every node. The order is the order they were stored in and carries
    /// nothing, since every edge is a handle.
    pub nodes: Vec<Node>,
    /// Every way in.
    pub roots: Vec<Root>,
}

/// The 8 byte header in front of FLW1's node table: how many records follow,
/// and how many entries the indirection table past them holds.
mod flw1_offsets {
    pub const LEN: usize = 0x08;
    pub const NODE_COUNT: usize = 0x00;
    pub const TABLE_COUNT: usize = 0x02;
    // 0x04, 4 bytes: padding.
}

/// One FLW1 node record: 8 bytes whatever the type, laid out differently
/// per type from byte 1 on.
mod node_record {
    pub const LEN: usize = 0x08;
    pub const TYPE: usize = 0x00;

    // Identifiers at TYPE
    pub const TEXT: u8 = 0x01;
    pub const BRANCH: u8 = 0x02;
    pub const EVENT: u8 = 0x03;

    // Doesn't need the indirection table, as indirection and mask bytes can
    // live in the record itself
    pub mod text {
        // 0x01 unused
        pub const MESSAGE: usize = 0x02;
        pub const NEXT: usize = 0x04;
        /// The same mask byte the indirection table trails, for `NEXT`.
        pub const MASK: usize = 0x06;
        // 0x07, 1 byte: padding.
    }

    pub mod branch {
        pub const CHILD_COUNT: usize = 0x01;
        pub const QUERY: usize = 0x02;
        pub const PARAM: usize = 0x04;
        pub const TABLE_START: usize = 0x06;
    }

    pub mod event {
        pub const EVENT: usize = 0x01;
        pub const TABLE_INDEX: usize = 0x02;
        pub const PARAMS: usize = 0x04;
    }
}

/// The 8 byte header in front of FLI1's root table.
mod fli1_offsets {
    pub const LEN: usize = 0x08;
    pub const COUNT: usize = 0x00;
    /// The entry width, always 8. Not read, since the stride below is fixed
    /// either way, but written.
    pub const ENTRY_LEN: usize = 0x02;
    // 0x03, 5 bytes: padding.
}

/// One FLI1 entry: an id and the node position it starts at, each padded out
/// to a u32 of its own.
mod fli1_entry {
    pub const LEN: usize = 0x08;
    pub const FLOW_ID: usize = 0x00;
    // 0x02, 2 bytes: padding.
    pub const NODE: usize = 0x04;
    // 0x06, 2 bytes: padding.
}

/// What marks a dead end wherever an edge is stored: a node record's own
/// next field, or a slot in the indirection table.
const DEAD_END: u16 = 0xFFFF;

/// Every array inside FLW1 is padded to a multiple of this many bytes.
const ALIGN: usize = 16;

/// One indirection table entry: a node position, or [`DEAD_END`].
const INDIR_TABLE_ENTRY_LEN: usize = 2;

/// Turns a table entry, or a record's own edge field, into an edge:
/// [`DEAD_END`] is no edge, anything else is the node at that position.
fn edge(entry: u16) -> Option<NodeId> {
    (entry != DEAD_END).then_some(NodeId(entry as u32))
}

/// The mask byte that shadows a stored edge: set for a dead end, clear for
/// a node.
const fn mask(entry: u16) -> u8 {
    if entry == DEAD_END { 0xFF } else { 0x00 }
}

/// Reads the graph out of the two sections that hold it.
///
/// FLW1's indirection table is read first: its position falls straight out
/// of the header, with no need to have read a single node record. Its
/// trailing mask is checked against it before anything else happens, since
/// the game never reads the mask back, but a mismatch, or its absence,
/// means the file is corrupt. Node records are then read in order, each
/// resolving its own branch or event edges out of the table as it goes.
/// FLI1 is then read straight into [`Root`]s.
// `flw1`/`fli1` mirror the file format's own FLW1/FLI1 section names.
#[allow(clippy::similar_names)]
pub fn read(flw1: &[u8], fli1: &[u8]) -> Result<Flow> {
    let flw = Reader::new(flw1);
    let node_count = flw.u16_at(flw1_offsets::NODE_COUNT)? as usize;
    let table_count = flw.u16_at(flw1_offsets::TABLE_COUNT)? as usize;

    let records = flw.slice_at(flw1_offsets::LEN, node_count * node_record::LEN)?;
    let table_at = flw1_offsets::LEN + records.len();
    let mut table = Vec::with_capacity(table_count);
    for i in 0..table_count {
        table.push(flw.u16_at(table_at + i * INDIR_TABLE_ENTRY_LEN)?);
    }

    // The mask is never dereferenced by the game, so nothing downstream
    // needs it, but every file has one. It is read and checked against the
    // table it shadows, since disagreement here is the one free corruption
    // check this section offers.
    let mask_at = table_at + table_count * INDIR_TABLE_ENTRY_LEN;
    if flw1.len() < mask_at + table_count {
        return Err(Error::Corrupt(
            "a flow indirection table has no mask table trailing it",
        ));
    }
    for (i, &entry) in table.iter().enumerate() {
        if flw.u8_at(mask_at + i)? != mask(entry) {
            return Err(Error::Corrupt(
                "a flow indirection table entry disagrees with its own mask byte",
            ));
        }
    }

    let mut nodes = Vec::with_capacity(node_count);
    // A node's id is its position, counted in the id's own width rather than
    // narrowed out of a `usize` index.
    let (records, _) = records.as_chunks::<{ node_record::LEN }>();
    for (id, record) in (0..).zip(records) {
        let rec = Reader::new(record);
        let id = NodeId(id);

        match record[node_record::TYPE] {
            node_record::TEXT => {
                let message = rec.u16_at(node_record::text::MESSAGE)?;
                let next = rec.u16_at(node_record::text::NEXT)?;
                if rec.u8_at(node_record::text::MASK)? != mask(next) {
                    return Err(Error::Corrupt(
                        "a text node's edge disagrees with its own mask byte",
                    ));
                }
                nodes.push(Node::Text {
                    id,
                    message: MessageId(message as u32),
                    next: edge(next),
                });
            }
            node_record::BRANCH => {
                let query = rec.u16_at(node_record::branch::QUERY)?;
                let param = rec.u16_at(node_record::branch::PARAM)?;
                let table_start = rec.u16_at(node_record::branch::TABLE_START)? as usize;
                let count = record[node_record::branch::CHILD_COUNT] as usize;

                let end = table_start + count;
                let slice = table.get(table_start..end).ok_or(Error::Corrupt(
                    "a branch's children run past the indirection table",
                ))?;
                nodes.push(Node::Branch {
                    id,
                    query,
                    param,
                    children: slice.iter().map(|&entry| edge(entry)).collect(),
                });
            }
            node_record::EVENT => {
                let table_index = rec.u16_at(node_record::event::TABLE_INDEX)? as usize;
                let params = rec.bytes_at(node_record::event::PARAMS)?;

                let entry = *table.get(table_index).ok_or(Error::Corrupt(
                    "an event's next pointer is past the indirection table",
                ))?;
                nodes.push(Node::Event {
                    id,
                    event: record[node_record::event::EVENT],
                    params,
                    next: edge(entry),
                });
            }
            _ if id.0 as usize == node_count - 1 => {
                // The one padding record a file is allowed, there only to
                // round an odd node count up to even. Not a node.
            }
            _ => {
                return Err(Error::Corrupt(
                    "a flow node's type is not text, branch, or event before the end of the node table",
                ));
            }
        }
    }

    let fli = Reader::new(fli1);
    let root_count = fli.u16_at(fli1_offsets::COUNT)? as usize;
    let entries = fli.slice_at(fli1_offsets::LEN, root_count * fli1_entry::LEN)?;
    let (entries, _) = entries.as_chunks::<{ fli1_entry::LEN }>();
    let roots = entries
        .iter()
        .map(|entry| {
            let entry = Reader::new(entry);
            Ok(Root {
                public_id: entry.u16_at(fli1_entry::FLOW_ID)?,
                node: NodeId(entry.u16_at(fli1_entry::NODE)? as u32),
            })
        })
        .collect::<Result<_>>()?;

    Ok(Flow { nodes, roots })
}

/// FLW1 and FLI1, back from what [`read`] took apart.
///
/// Nodes are numbered by position in [`Flow::nodes`], and every edge and
/// root is written as that number. Text nodes name their message by its
/// position too, which is what `messages` maps a handle to. The indirection
/// table is filled in node order, a branch's children in a run and an
/// event's next pointer on its own, which is where the retail files put them.
// `flw1`/`fli1` mirror the file format's own FLW1/FLI1 section names.
#[allow(clippy::similar_names)]
pub fn write(flow: &Flow, messages: &HashMap<MessageId, u16>) -> Result<(Vec<u8>, Vec<u8>)> {
    let nodes = positions(
        flow.nodes.iter().map(Node::id),
        "two flow nodes share an id",
    )?;
    let position = |node: NodeId| {
        nodes.get(&node).copied().ok_or(Error::Unwritable(
            "a flow edge or root points at a node the graph does not hold",
        ))
    };
    let entry = |edge: Option<NodeId>| edge.map_or(Ok(DEAD_END), position);

    let record_count = flow.nodes.len().next_multiple_of(ALIGN / node_record::LEN);
    let mut flw1 = Writer::with_capacity(flw1_offsets::LEN + record_count * node_record::LEN);
    flw1.zeros(flw1_offsets::LEN);
    flw1.u16_at(
        flw1_offsets::NODE_COUNT,
        u16::try_from(record_count).map_err(|_| Error::Oversized)?,
    );

    let mut table = Vec::new();
    for node in &flow.nodes {
        let at = flw1.len();
        flw1.zeros(node_record::LEN);
        match node {
            Node::Text { message, next, .. } => {
                let message = *messages.get(message).ok_or(Error::Unwritable(
                    "a text node displays a message the file does not hold",
                ))?;
                let next = entry(*next)?;
                flw1.u8_at(at + node_record::TYPE, node_record::TEXT);
                flw1.u16_at(at + node_record::text::MESSAGE, message);
                flw1.u16_at(at + node_record::text::NEXT, next);
                flw1.u8_at(at + node_record::text::MASK, mask(next));
            }
            Node::Branch {
                query,
                param,
                children,
                ..
            } => {
                let count = u8::try_from(children.len())
                    .map_err(|_| Error::Unwritable("a branch has more than 255 answers"))?;
                let start = u16::try_from(table.len()).map_err(|_| Error::Oversized)?;
                for &child in children {
                    table.push(entry(child)?);
                }
                flw1.u8_at(at + node_record::TYPE, node_record::BRANCH);
                flw1.u8_at(at + node_record::branch::CHILD_COUNT, count);
                flw1.u16_at(at + node_record::branch::QUERY, *query);
                flw1.u16_at(at + node_record::branch::PARAM, *param);
                flw1.u16_at(at + node_record::branch::TABLE_START, start);
            }
            Node::Event {
                event,
                params,
                next,
                ..
            } => {
                let index = u16::try_from(table.len()).map_err(|_| Error::Oversized)?;
                table.push(entry(*next)?);
                flw1.u8_at(at + node_record::TYPE, node_record::EVENT);
                flw1.u8_at(at + node_record::event::EVENT, *event);
                flw1.u16_at(at + node_record::event::TABLE_INDEX, index);
                flw1.bytes_at(at + node_record::event::PARAMS, params);
            }
        }
    }
    // The padding record, when the count is odd.
    flw1.zeros((record_count - flow.nodes.len()) * node_record::LEN);

    table.resize(
        table.len().next_multiple_of(ALIGN / INDIR_TABLE_ENTRY_LEN),
        0,
    );
    flw1.u16_at(
        flw1_offsets::TABLE_COUNT,
        u16::try_from(table.len()).map_err(|_| Error::Oversized)?,
    );
    for &entry in &table {
        flw1.u16(entry);
    }
    for &entry in &table {
        flw1.u8(mask(entry));
    }

    let mut fli1 = Writer::with_capacity(fli1_offsets::LEN + flow.roots.len() * fli1_entry::LEN);
    fli1.zeros(fli1_offsets::LEN);
    fli1.u16_at(
        fli1_offsets::COUNT,
        u16::try_from(flow.roots.len()).map_err(|_| Error::Oversized)?,
    );
    fli1.u8_at(fli1_offsets::ENTRY_LEN, 8);
    for root in &flow.roots {
        let at = fli1.len();
        fli1.zeros(fli1_entry::LEN);
        fli1.u16_at(at + fli1_entry::FLOW_ID, root.public_id);
        fli1.u16_at(at + fli1_entry::NODE, position(root.node)?);
    }

    Ok((flw1.finish(), fli1.finish()))
}

#[cfg(test)]
// `flw1`/`fli1` mirror the file format's own FLW1/FLI1 section names.
#[allow(clippy::similar_names)]
mod tests {
    use super::*;

    /// One of each node type, with the odd count that forces the trailing
    /// padding record, a two-way branch, and both a live and a dead edge in
    /// the table. Bytes lifted from the same fixture the writer will be
    /// checked against, so the two stay honest with each other.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let flw1 = [
            0x00, 0x04, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, // header
            0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, // Text -> node 1
            0x02, 0x02, 0x00, 0x23, 0x00, 0x00, 0x00, 0x00, // Branch, table @0
            0x03, 0x08, 0x00, 0x02, 0x0b, 0x00, 0x00, 0x00, // Event, table @2
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
            0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, // table: 0, dead, dead, 0
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // table: 0, 0, 0, 0
            0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, // mask, one per table entry
        ]
        .to_vec();
        let fli1 = [
            0x00, 0x02, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, // header
            0x0b, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // flow 3000 -> node 0
            0x0b, 0xb9, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, // flow 3001 -> node 2
        ]
        .to_vec();
        (flw1, fli1)
    }

    #[test]
    fn every_node_type_and_both_roots_are_read() {
        let (flw1, fli1) = sample();
        let flow = read(&flw1, &fli1).unwrap();

        assert_eq!(flow.nodes.len(), 3);
        assert!(matches!(
            flow.nodes[0],
            Node::Text {
                id: NodeId(0),
                message: MessageId(0),
                next: Some(NodeId(1)),
            }
        ));
        let Node::Branch {
            id,
            query,
            param,
            children,
        } = &flow.nodes[1]
        else {
            panic!("expected a branch node");
        };
        assert_eq!(*id, NodeId(1));
        assert_eq!(*query, 35);
        assert_eq!(*param, 0);
        assert_eq!(children, &[Some(NodeId(0)), None]);
        let Node::Event {
            id,
            event,
            params,
            next,
        } = &flow.nodes[2]
        else {
            panic!("expected an event node");
        };
        assert_eq!(*id, NodeId(2));
        assert_eq!(*event, 8);
        assert_eq!(*params, [11, 0, 0, 0]);
        assert_eq!(*next, None);

        assert_eq!(flow.roots.len(), 2);
        assert_eq!(flow.roots[0].public_id, 3000);
        assert_eq!(flow.roots[0].node, NodeId(0));
        assert_eq!(flow.roots[1].public_id, 3001);
        assert_eq!(flow.roots[1].node, NodeId(2));
    }

    /// The padding record is only tolerated as the very last one; the same
    /// unrecognised type earlier in the table is a corrupt file, not more
    /// padding.
    #[test]
    fn an_unrecognised_type_before_the_end_is_corrupt() {
        let (mut flw1, fli1) = sample();
        flw1[8] = 0x00; // the first (Text) record's type, now unrecognised
        assert!(matches!(read(&flw1, &fli1), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_mask_byte_disagreeing_with_its_entry_is_corrupt() {
        let (mut flw1, fli1) = sample();
        let mask_at = flw1.len() - 8;
        flw1[mask_at] = 0xff; // entry 0 is live (0x0000), mask now says dead
        assert!(matches!(read(&flw1, &fli1), Err(Error::Corrupt(_))));

        let (mut flw1, fli1) = sample();
        flw1[14] = 0xff; // the text node's next is live, its mask now says dead
        assert!(matches!(read(&flw1, &fli1), Err(Error::Corrupt(_))));
    }

    /// The fixture back byte for byte: the odd node count padded to even,
    /// the three table entries rounded up to eight, and the mask bytes
    /// trailing them.
    #[test]
    fn the_graph_is_written_the_way_it_was_read() {
        let (flw1, fli1) = sample();
        let flow = read(&flw1, &fli1).unwrap();
        let messages = HashMap::from([(MessageId(0), 0)]);
        assert_eq!(write(&flow, &messages).unwrap(), (flw1, fli1));
    }

    /// Handles are stable, so a node that was shuffled or dropped is found by
    /// its handle rather than by whatever now sits at its old position.
    #[test]
    fn edges_follow_handles_and_a_dangling_one_is_unwritable() {
        let (flw1, fli1) = sample();
        let mut flow = read(&flw1, &fli1).unwrap();
        let messages = HashMap::from([(MessageId(0), 0)]);

        flow.nodes.swap(0, 2);
        let (flw1, _) = write(&flow, &messages).unwrap();
        // Node 0 is now the event, at position 0; the text node at position
        // 2 still points at the branch, which is still at position 1.
        assert_eq!(flw1[8], node_record::EVENT);
        assert_eq!(
            &flw1[24..32],
            [0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
        );

        flow.nodes.remove(1);
        assert!(matches!(write(&flow, &messages), Err(Error::Unwritable(_))));
    }

    #[test]
    fn a_message_the_file_does_not_hold_is_unwritable() {
        let (flw1, fli1) = sample();
        let flow = read(&flw1, &fli1).unwrap();
        assert!(matches!(
            write(&flow, &HashMap::new()),
            Err(Error::Unwritable(_))
        ));
    }

    /// The game never reads the mask table back, but a writer can't leave it
    /// out: every real file has one, so a file with no room for it is
    /// corrupt rather than merely mask-less.
    #[test]
    fn a_file_with_no_room_for_a_mask_table_is_corrupt() {
        let (flw1, fli1) = sample();
        let truncated = &flw1[..flw1.len() - 8];
        assert!(matches!(read(truncated, &fli1), Err(Error::Corrupt(_))));
    }

    /// A branch's own record passed the mask check, since that only looks at
    /// the table entries themselves, so this is caught only once its
    /// children are actually resolved.
    #[test]
    fn a_branchs_children_running_past_the_indirection_table_is_corrupt() {
        let (mut flw1, fli1) = sample();
        flw1[23] = 0x07; // the branch's table_start, now 7 with a count of 2
        assert!(matches!(read(&flw1, &fli1), Err(Error::Corrupt(_))));
    }

    #[test]
    fn an_events_table_index_past_the_indirection_table_is_corrupt() {
        let (mut flw1, fli1) = sample();
        flw1[27] = 0x08; // the event's table_index, now 8 against a table of 8
        assert!(matches!(read(&flw1, &fli1), Err(Error::Corrupt(_))));
    }
}
