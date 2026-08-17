//! The flow graph: FLW1 and FLI1.
//!
//! A message on its own is one box of text. A flow is what turns a pile of
//! them into a conversation: nodes that display a message, branch on something
//! the game knows, or fire an event, each pointing at what comes next.
//!
//! # Layout
//!
//! FLW1 holds every node as an eight byte record, then an indirection table
//! for edges that don't fit in the record: a branch owns one table entry per
//! answer, an event owns exactly one (its next pointer, spelled the long way
//! round). A text node's single edge stays in the record and never touches
//! the table. A dead end is marked 0xFFFF wherever the edge is stored.
//!
//! A mask byte per table entry follows. The game computes a pointer to it on
//! load but never dereferences it: dead ends are found by comparing table
//! entries against 0xFFFF directly. It's round-tripped as-is so a rebuilt
//! file matches the original byte for byte.
//!
//! FLI1 is the way in: one entry per [`Root`].

use crate::{MessageId, Result};

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
#[derive(Debug, Clone)]
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
    pub fn id(&self) -> NodeId {
        match self {
            Node::Text { id, .. } | Node::Branch { id, .. } | Node::Event { id, .. } => *id,
        }
    }
}

/// One way into the graph.
#[derive(Debug, Clone, Copy)]
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
#[derive(Debug, Clone, Default)]
pub struct Flow {
    /// Every node. The order is the order they were stored in and carries
    /// nothing, since every edge is a handle.
    pub nodes: Vec<Node>,
    /// Every way in.
    pub roots: Vec<Root>,
}

/// Reads the graph out of the two sections that hold it.
pub(crate) fn read(_flw1: &[u8], _fli1: &[u8]) -> Result<Flow> {
    todo!("FLW1 nodes and edges, FLI1 roots")
}
