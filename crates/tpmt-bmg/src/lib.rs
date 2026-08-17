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

mod flow;
mod message;

pub use crate::flow::Flow;
pub use crate::message::{Message, MessageId, Mid1Header, TextSegment};

use tpmt_bytes::Reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a BMG message file")]
    NotBmg,

    #[error("the message file is corrupt: {0}")]
    Corrupt(&'static str),

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
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
#[derive(Debug, Clone)]
pub struct UnknownSection {
    pub magic: [u8; 4],
    pub data: Vec<u8>,
}

/// A message file taken apart.
#[derive(Debug, Clone)]
pub struct Bmg {
    pub encoding: Encoding,
    /// How wide one INF1 record is, the 4 byte text offset at the front of it
    /// included, so it is four more than an attribute record's own length.
    ///
    /// Carried rather than derived because a file with no messages still
    /// states one, and because a length the game does not recognise is a real
    /// thing some files have.
    pub attribute_len: u8,
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

/// The sections a file holds, sorted out by name on the way past.
/// INF1 and DAT1 are always there, so they are required fields rather than
/// `Option`s left to be checked after the fact.
struct Sections<'a> {
    inf1: &'a [u8],
    dat1: &'a [u8],
    mid1: Option<&'a [u8]>,
    str1: Option<&'a [u8]>,
    /// FLW1 and FLI1 together, which are always both present or both absent.
    /// Paired up here rather than kept as two fields so that state is
    /// unrepresentable instead of checked for.
    flow: Option<(&'a [u8], &'a [u8])>,
    extra: Vec<UnknownSection>,
}

/// Walks the section table, sorting every section out by name.
///
/// Each section says how far the next one is, so the table is walked from the
/// front. A count larger than the file holds runs out of bytes on the section
/// it invents, rather than being trusted as an allocation.
fn split(data: &[u8]) -> Result<(Encoding, Sections<'_>)> {
    let reader = Reader::new(data);
    let encoding = Encoding::from_byte(reader.u8_at(header::ENCODING)?);
    let count = reader.u32_at(header::SECTION_COUNT)? as usize;

    let mut at = header::LEN;
    // What the header should have stated: the flow sections are left out of
    // it, so they are left out of this too.
    let mut stated = header::LEN;
    let mut inf1 = None;
    let mut dat1 = None;
    let mut mid1 = None;
    let mut str1 = None;
    let mut flw1 = None;
    let mut fli1 = None;
    let mut extra = Vec::new();

    for _ in 0..count {
        let magic: [u8; 4] = reader
            .slice_at(at, 4)?
            .try_into()
            .expect("four bytes are four bytes");
        let size = reader.u32_at(at + section::SIZE)? as usize;
        if size < section::HEADER_LEN {
            return Err(Error::Corrupt("a section is smaller than its own header"));
        }

        // The last section in a file is allowed to stop where the file does,
        // with the padding its stated size counts left off the end.
        let body_at = at + section::HEADER_LEN;
        let len = (size - section::HEADER_LEN).min(data.len() - body_at);
        let body = reader.slice_at(body_at, len)?;

        match &magic {
            b"INF1" => inf1 = Some(body),
            b"DAT1" => dat1 = Some(body),
            b"MID1" => mid1 = Some(body),
            b"STR1" => str1 = Some(body),
            b"FLW1" => flw1 = Some(body),
            b"FLI1" => fli1 = Some(body),
            _ => extra.push(UnknownSection {
                magic,
                data: body.to_vec(),
            }),
        }
        at += size;
        if !matches!(&magic, b"FLW1" | b"FLI1") {
            stated += size;
        }
    }

    // The one number in the file that says anything about the rest of it, so
    // it is checked rather than skipped: a walk that ends somewhere else read
    // a size wrong, or the file is not laid out the way it claims.
    if reader.u32_at(header::SIZE)? as usize != stated {
        return Err(Error::Corrupt(
            "the stated size is not where the sections end",
        ));
    }

    let flow = match (flw1, fli1) {
        (Some(flw1), Some(fli1)) => Some((flw1, fli1)),
        (None, None) => None,
        _ => {
            return Err(Error::Corrupt(
                "the flow graph is missing one of its two sections",
            ));
        }
    };

    Ok((
        encoding,
        Sections {
            inf1: inf1.ok_or(Error::Corrupt("there is no message table"))?,
            dat1: dat1.ok_or(Error::Corrupt("there is no message text"))?,
            mid1,
            str1,
            flow,
            extra,
        },
    ))
}

/// Takes a message file apart.
pub fn unpack(data: &[u8]) -> Result<Bmg> {
    if !data.starts_with(header::MAGIC) {
        return Err(Error::NotBmg);
    }

    let (encoding, sections) = split(data)?;

    let (messages, attribute_len) =
        message::read_messages(sections.inf1, sections.dat1, sections.mid1)?;
    Ok(Bmg {
        encoding,
        attribute_len,
        mid1: sections.mid1.map(message::read_mid1).transpose()?,
        messages,
        flow: sections
            .flow
            .map(|(flw1, fli1)| flow::read(flw1, fli1))
            .transpose()?,
        strings: sections.str1.map(read_strings),
        extra: sections.extra,
    })
}

/// Writes a whole message file from what [`unpack`] took apart.
pub fn pack(_bmg: &Bmg) -> Result<Vec<u8>> {
    todo!("the writer")
}

/// The string pool split on its terminators.
///
/// Splitting and rejoining are the same operation backwards, so trailing empty
/// entries are kept rather than trimmed: they are how a pool that ends in
/// several terminators comes back as the bytes it was.
fn read_strings(str1: &[u8]) -> Vec<Vec<u8>> {
    str1.split(|&byte| byte == 0).map(<[u8]>::to_vec).collect()
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

    /// A file of `magics`, one 0x10 section each, stating `size` for them.
    fn file(size: u32, magics: &[&[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(header::MAGIC);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&(magics.len() as u32).to_be_bytes());
        out.push(Encoding::ShiftJis.byte());
        out.resize(header::LEN, 0);
        for magic in magics {
            out.extend_from_slice(*magic);
            out.extend_from_slice(&0x10u32.to_be_bytes());
            out.resize(out.len() + 8, 0);
        }
        out
    }

    /// The stated size covers the header and every section but the flow pair,
    /// which is what makes it worth checking: a file stating its whole length
    /// instead is one whose sections are not where it says they are.
    ///
    /// Only the walk is exercised, since taking the sections apart is still todo
    #[test]
    fn the_stated_size_leaves_the_flow_sections_out() {
        let magics = [b"INF1", b"DAT1", b"FLW1", b"FLI1"];
        let data = file(0x40, &magics);
        let (encoding, sections) = split(&data).unwrap();
        assert_eq!(encoding, Encoding::ShiftJis);
        assert!(sections.flow.is_some());

        // 0x60 is the whole file, flow sections and all.
        let whole = file(0x60, &magics);
        assert!(matches!(split(&whole), Err(Error::Corrupt(_))));
    }

    /// A section nothing here knows still counts toward the size, and is kept
    /// whole rather than skipped.
    #[test]
    fn an_unknown_section_is_kept_and_counted() {
        let data = file(0x50, &[b"INF1", b"DAT1", b"XXXX"]);
        let (_, sections) = split(&data).unwrap();
        assert_eq!(sections.extra.len(), 1);
        assert_eq!(sections.extra[0].magic, *b"XXXX");
    }

    #[test]
    fn the_string_pool_keeps_its_empty_entries() {
        assert_eq!(
            read_strings(b"\0arrow\0arrows\0"),
            [b"".as_slice(), b"arrow", b"arrows", b""]
        );
    }
}
