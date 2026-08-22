//! BMG binary message files.
//!
//! Every line of dialogue, item name, and sign in the game. A BMG is a pool of
//! Shift-JIS strings alongside a table of fixed-width attribute records, one
//! per message, whose field layout varies from file to file. The strings may carry
//! inline tags for colour, ruby text, button glyphs, and control flow.
//!
//! Additionally, BMGs also allow for connecting a flow graph between message nodes,
//! allowing for messages to flow seemlessly between each other. This flow may
//! branch based on conditions, or emit events which do certain actions (such as
//! giving the player an item, setting flags, or triggering screen effects).
//!
//! Container only. What a tag means, what an event does, and which attribute
//! byte holds the speaker are all per-game facts, and none of them are needed
//! to take a file apart and put it back together.
//!
//! # Layout
//!
//! ```text
//! 0x00  header      the magic, a size, and how many sections follow
//! 0x20  sections    one after another, each naming itself and its own size
//! ```
//!
//! Every section states its own padded size, so the table is walked rather
//! than indexed, and a name nothing here knows is stepped over intact:
//!
//! ```text
//! INF1  one fixed-width record per message: where its text starts, then the
//!       attribute bytes saying how the message is displayed
//! DAT1  the text itself, every message null terminated
//! MID1  the ids the game looks messages up by, one per INF1 record
//! STR1  a pool of null terminated strings that attributes point into
//! FLW1  the flow graph: one record per node, and the edge table they index
//! FLI1  which node each flow id enters the graph at
//! ```
//!
//! Only INF1 and DAT1 are always there. A file addressed positionally carries
//! no MID1, and one nothing branches through carries neither flow section.

pub mod editable;
mod pack;
mod sections;
mod unpack;

pub use crate::sections::flow::Flow;
pub use crate::sections::message::{Message, MessageId, Mid1Header, TextSegment};
pub use pack::pack;
pub use unpack::unpack;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a BMG message file")]
    NotBmg,

    #[error("the message file is corrupt: {0}")]
    Corrupt(&'static str),

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),

    #[error("not a BMG translation document: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

mod header {
    pub const LEN: usize = 0x20;
    pub const MAGIC: &[u8; 8] = b"MESGbmg1";
    /// The size of the file with the flow sections left out.
    pub const SIZE: usize = 0x08;
    pub const SECTION_COUNT: usize = 0x0C;
    pub const ENCODING: usize = 0x10;
    // The rest of the header, 0x11 to the end of it, is zero.
}

/// What every section opens with: a four character name, then the size of the
/// whole section, its own header and trailing padding included.
mod section {
    pub const HEADER_LEN: usize = 0x08;
    // 0x00 - Name/magic
    pub const SIZE: usize = 0x04;
}

/// Which encoding the bmg text is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// What a file predating the byte itself is in, which is whatever the game
    /// reading it assumed.
    Legacy = 0x00,
    Windows1252 = 0x01,
    Utf16Be = 0x02,
    /// The game's own default, and what every retail file in it is in.
    #[default]
    ShiftJis = 0x03,
    Utf8 = 0x04,
}

impl Encoding {
    /// Reads the header byte. An unknown one is Shift-JIS, the same guess the
    /// game makes rather than a reason to refuse the file.
    pub fn from_byte(byte: u8) -> Encoding {
        match byte {
            0x00 => Encoding::Legacy,
            0x01 => Encoding::Windows1252,
            0x02 => Encoding::Utf16Be,
            0x04 => Encoding::Utf8,
            _ => Encoding::ShiftJis,
        }
    }

    pub fn byte(self) -> u8 {
        self as u8
    }

    /// The name the editable form spells it as.
    pub fn as_str(self) -> &'static str {
        match self {
            Encoding::Legacy => "legacy-bmg",
            Encoding::Windows1252 => "windows-1252",
            Encoding::Utf16Be => "utf-16be",
            Encoding::ShiftJis => "shift-jis",
            Encoding::Utf8 => "utf-8",
        }
    }
}

impl std::fmt::Display for Encoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A section this crate has no implementation about, kept whole so it goes back out
/// exactly as it came in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSection {
    pub magic: [u8; 4],
    pub data: Vec<u8>,
}

/// A message file taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bmg {
    pub encoding: Encoding,
    /// How wide one INF1 record is, the 4 byte text offset at the front of it
    /// included, so it is four more than an attribute record's own length.
    ///
    /// Carried rather than derived because a file with no messages still
    /// states one, and because a length the game does not recognise is a real
    /// thing some files have.
    pub attribute_len: u16,
    /// `None` for a file with no MID1 at all, which is addressed by position.
    pub mid1: Option<Mid1Header>,
    pub messages: Vec<Message>,
    /// FLW1 and FLI1 together, which are always both there or both absent.
    pub flow: Option<Flow>,
    /// STR1: a flat pool of null terminated strings that attributes point into
    /// by byte offset, such as common used item names. One entry per string,
    /// terminators excluded.
    pub strings: Option<Vec<Vec<u8>>>,
    /// Every section none of the above named.
    pub extra: Vec<UnknownSection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anything_else_is_not_a_message_file() {
        assert!(matches!(unpack(b"RARC"), Err(Error::NotBmg)));
        assert!(matches!(unpack(b""), Err(Error::NotBmg)));
    }

    /// The header byte and the variant it names, both ways round. An unknown
    /// byte reads as the game's default rather than failing, and writes back
    /// out as that default's own byte.
    #[test]
    fn the_encoding_byte_is_read_and_written_the_same_way() {
        for byte in 0x00..=0x04 {
            assert_eq!(Encoding::from_byte(byte).byte(), byte);
        }
        assert_eq!(Encoding::from_byte(0xFF), Encoding::ShiftJis);
        assert_eq!(Encoding::from_byte(0xFF).byte(), 0x03);
    }
}
