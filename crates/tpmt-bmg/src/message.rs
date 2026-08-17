//! Message text and attributes: INF1, DAT1, and MID1.
//!
//! One message is spread across three sections: its fixed-width attribute
//! record in INF1, the text that record points at in DAT1, and, when the
//! file has one, the id sitting at the same position in MID1.

use crate::Result;

/// Stable internal handle for a message, held by whatever refers to one.
///
/// Callers address a message by [`Message::public_id`], not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(pub u32);

/// One stretch of a message's text.
///
/// Text and tags are parsed (seperated), but neither decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextSegment {
    Text(Vec<u8>),
    /// One escape sequence whole, its leading 0x1A and length byte included.
    Tag(Vec<u8>),
}

/// One message: what it is called, how it is displayed, and what it says.
#[derive(Debug, Clone)]
pub struct Message {
    /// The id external callers look this message up by. Held in MID1, and
    /// duplicated in the first two bytes of attributes.
    ///
    /// Meaningless when the file has no [`crate::Bmg::mid1`], since such a
    /// file is addressed by position instead.
    ///
    /// An id above 5000 is redirected to a different resource entirely on
    /// every display path the game has, in world and on the HUD alike. See
    /// [`crate::Flow::roots`] for the separate and unrelated threshold flow
    /// ids have.
    pub public_id: u16,
    /// Internal id for this message, see [`MessageId`].
    pub id: MessageId,
    /// The attributes as stored: animation, sound, box style and the rest of it.
    /// Which byte is which is game data, so it stays raw here.
    pub attributes: Vec<u8>,
    pub text: Vec<TextSegment>,
}

/// What it says about the id lookup array that follows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mid1Header {
    /// Fast path if ids are sorted (binary search) vs scanning.
    /// Packed into the same byte as `form` in the file (high nibble).
    pub ordered: bool,
    /// Which layout the id array is in.
    /// Packed into the low nibble beside `ordered`.
    /// The game asserts this is zero and never branches on it (sanity check).
    pub form: u8,
    /// Bytes a stored id shifts left to make room for a second value packed
    /// in beside it, for looking a message up by two numbers at once (say,
    /// an item and a variant) instead of one flat id. Left unimplemented:
    /// no file exercises it, so there is nothing to check a decoding against,
    /// and it is kept verbatim rather than guessed at.
    pub shift_bytes: u8,
}

/// The messages, and how wide one INF1 record is.
///
/// All three sections are read together because one message is spread over all
/// of them: its record in INF1, the text that record points at in DAT1, and
/// the id sitting at the same position in MID1.
pub(crate) fn read_messages(
    _inf1: &[u8],
    _dat1: &[u8],
    _mid1: Option<&[u8]>,
) -> Result<(Vec<Message>, u8)> {
    todo!("INF1 records, DAT1 text, MID1 ids")
}

/// What MID1 says about its ids, as against the ids themselves.
pub(crate) fn read_mid1(_mid1: &[u8]) -> Result<Mid1Header> {
    todo!("the MID1 header")
}
