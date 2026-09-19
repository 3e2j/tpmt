//! The sections this crate parses into structured data: message text and
//! attributes, and the flow graph connecting them. Everything else stays raw
//! bytes.
//!
//! Each section module holds its layout once, and reads and writes it from
//! the same constants, so the two directions cannot drift apart.

use std::collections::HashMap;
use std::hash::Hash;

use crate::{Error, Result};

pub mod flow;
pub mod message;

// What every section opens with: a four character name, then the size of the
// whole section, its own header and trailing padding included.
pub const HEADER_LEN: usize = 0x08;
// 0x00 - Name/magic
pub const SIZE: usize = 0x04;
/// Every section's stated size is padded to this, and every section but
/// the last in the file is written padded out to it.
pub const ALIGN: usize = 0x20;

/// Message information (table): one fixed-width attribute record per message. See [`message`].
pub const INF1: [u8; 4] = *b"INF1";
/// Message data (text) that INF1 records point into. See [`message`].
pub const DAT1: [u8; 4] = *b"DAT1";
/// Message IDs the game looks messages up by, one per INF1 record. See [`message`].
pub const MID1: [u8; 4] = *b"MID1";
/// A pool of null terminated strings that attributes point into. Kept raw.
pub const STR1: [u8; 4] = *b"STR1";
/// Flow graph's nodes and edges. See [`flow`].
pub const FLW1: [u8; 4] = *b"FLW1";
/// Flow graph's root IDs, the way in. See [`flow`].
pub const FLI1: [u8; 4] = *b"FLI1";

/// Maps each handle to its position in the sequence, which is how the file
/// stores a reference to a message or a node. Positions are worked out here
/// rather than read off the handles, which were positions once but need not
/// be any more.
///
/// `duplicate_error_msg` is the error for two handles that are the same,
/// since a reference to one of them could then go either way.
pub fn positions<Id: Copy + Eq + Hash>(
    ids: impl ExactSizeIterator<Item = Id>,
    duplicate_error_msg: &'static str,
) -> Result<HashMap<Id, u16>> {
    let count = u16::try_from(ids.len()).map_err(|_| Error::Oversized)?;
    let positions: HashMap<_, _> = ids.zip(0..count).collect();
    // A duplicate collapses two entries into one, so the map comes up short.
    if positions.len() != usize::from(count) {
        return Err(Error::Unwritable(duplicate_error_msg));
    }
    Ok(positions)
}
