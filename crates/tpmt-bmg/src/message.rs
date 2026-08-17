//! Message text and attributes: INF1, DAT1, and MID1.
//!
//! One message is spread across three sections: its fixed-width attribute
//! record in INF1, the text that record points at in DAT1, and, when the
//! file has one, the public-facing id sitting at the same position in MID1.

use tpmt_bytes::Reader;

use crate::{Error, Result};

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
    /// Bytes a stored id shifts left to make room for a second, independently
    /// packed id below it (say, an item and a variant). Always zero in
    /// practice, so a nonzero value is rejected on read rather than guessed
    /// at. See `read_mid1`.
    pub shift_bytes: u8,
}

/// The 8 byte header in front of INF1's records: how many there are, and how
/// wide one is.
mod inf1_header {
    /// How wide the header is, so also where its records start.
    pub const LEN: usize = 0x08;
    pub const COUNT: usize = 0x00;
    pub const RECORD_LEN: usize = 0x02;
    /// Not read: nothing here branches on which group a message belongs to.
    pub const _GROUP_ID: usize = 0x04;
    // 0x06, 2 bytes: padding.
}

/// The 8 byte header in front of MID1's id array.
mod mid1_header {
    /// How wide the header is, so also where the id array starts.
    pub const LEN: usize = 0x08;
    /// Not read: `count` is redundant with INF1's own record count, which is
    /// what the array is actually walked by.
    pub const _COUNT: usize = 0x00;
    /// High nibble `ordered`, low nibble `form`.
    pub const ORDERED_FORM: usize = 0x02;
    pub const SHIFT_BYTES: usize = 0x03;
}

/// The messages, and how wide one INF1 record is.
///
/// All three sections are read together because one message is spread over all
/// of them: its record in INF1, the text that record points at in DAT1, and
/// the id sitting at the same position in MID1.
pub(crate) fn read_messages(
    inf1: &[u8],
    dat1: &[u8],
    mid1: Option<&[u8]>,
) -> Result<(Vec<Message>, u16, Option<Mid1Header>)> {
    let mut reader = Reader::new(inf1);
    let count = reader.u16_at(inf1_header::COUNT)? as usize;
    // Text offset into DAT1 + attribute bytes
    let record_len = reader.u16_at(inf1_header::RECORD_LEN)?;
    let attributes_len = record_len.checked_sub(4).ok_or(Error::Corrupt(
        "an INF1 record is narrower than its own text offset",
    ))?;
    // Skip header (to records)
    reader.seek(inf1_header::LEN);

    // `shift_bytes` is guaranteed zero by `read_mid1`, so a MID1 entry is
    // always the id whole; see `Mid1Header::shift_bytes`.
    let mid1_header = mid1.map(read_mid1).transpose()?;
    let mid1 = mid1.map(Reader::new);

    let mut messages = Vec::with_capacity(count);
    for i in 0..count {
        let dat_offset = reader.u32()? as usize;
        let attributes = reader.take(attributes_len as usize)?.to_vec();

        let public_id = match &mid1 {
            Some(mid1) => {
                let entry = mid1.u32_at(mid1_header::LEN + i * 4)?;
                u16::try_from(entry)
                    .map_err(|_| Error::Corrupt("a MID1 id does not fit in 16 bits"))?
            }
            None => 0,
        };

        messages.push(Message {
            public_id,
            id: MessageId(i as u32),
            attributes,
            text: read_text(dat1, dat_offset)?,
        });
    }

    Ok((messages, record_len, mid1_header))
}

/// Splits one message's text at `start` into text and tag runs, stopping at
/// the terminating NUL. An empty run either side of a tag is left out: it
/// contributes nothing back when the segments are rejoined.
fn read_text(dat1: &[u8], start: usize) -> Result<Vec<TextSegment>> {
    let mut segments = Vec::new();
    let mut text_start = start;
    let mut i = start;

    loop {
        let byte = *dat1
            .get(i)
            .ok_or(Error::Corrupt("a message's text runs past the end of DAT1"))?;
        match byte {
            0x00 => {
                if i > text_start {
                    segments.push(TextSegment::Text(dat1[text_start..i].to_vec()));
                }
                return Ok(segments);
            }
            0x1A => {
                if i > text_start {
                    segments.push(TextSegment::Text(dat1[text_start..i].to_vec()));
                }
                let len = *dat1
                    .get(i + 1)
                    .ok_or(Error::Corrupt("a tag is cut off before its length byte"))?
                    as usize;
                if len < 2 {
                    return Err(Error::Corrupt(
                        "a tag claims to be shorter than its own header",
                    ));
                }
                let end = i + len;
                let tag = dat1
                    .get(i..end)
                    .ok_or(Error::Corrupt("a tag runs past the end of DAT1"))?;
                segments.push(TextSegment::Tag(tag.to_vec()));
                i = end;
                text_start = end;
            }
            _ => i += 1,
        }
    }
}

/// What MID1 says about its ids, as against the ids themselves.
fn read_mid1(mid1: &[u8]) -> Result<Mid1Header> {
    let reader = Reader::new(mid1);
    let byte = reader.u8_at(mid1_header::ORDERED_FORM)?;
    let shift_bytes = reader.u8_at(mid1_header::SHIFT_BYTES)?;
    // No call to `TResource::toMessageIndex_messageID` anywhere in TP or the
    // JSystem library it comes from ever passes a second id, so there is
    // nothing to check a decoding of a nonzero value against. The array is a
    // flat `u32` either way, so it buys nothing in storage; it would only be
    // recovering an unused lookup shortcut, worth a second look if that ever
    // turns out to matter.
    if shift_bytes != 0 {
        return Err(Error::Corrupt(
            "a MID1 header packs a second id into the message id, which is unsupported",
        ));
    }
    Ok(Mid1Header {
        ordered: byte & 0xF0 != 0,
        form: byte & 0x0F,
        shift_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_tags_split_correctly() {
        let mut dat1 = b"Hi ".to_vec();
        dat1.extend([0x1A, 4, 0x01, 0x02]);
        dat1.extend(b" there\0");

        let segments = read_text(&dat1, 0).unwrap();
        assert_eq!(
            segments,
            vec![
                TextSegment::Text(b"Hi ".to_vec()),
                TextSegment::Tag(vec![0x1A, 4, 0x01, 0x02]),
                TextSegment::Text(b" there".to_vec()),
            ]
        );
    }

    /// A tag right at the start, another right after it, and nothing after
    /// the second: no text segment has anywhere to come from.
    #[test]
    fn empty_runs_around_tags_are_dropped() {
        let dat1 = [0x1A, 3, 0xAA, 0x1A, 3, 0xBB, 0x00];

        let segments = read_text(&dat1, 0).unwrap();
        assert_eq!(
            segments,
            vec![
                TextSegment::Tag(vec![0x1A, 3, 0xAA]),
                TextSegment::Tag(vec![0x1A, 3, 0xBB]),
            ]
        );
    }

    #[test]
    fn text_without_terminator_is_corrupt() {
        assert!(matches!(read_text(b"abc", 0), Err(Error::Corrupt(_))));
    }

    #[test]
    fn tag_cut_off_before_length_byte_is_corrupt() {
        assert!(matches!(
            read_text(&[b'a', 0x1A], 0),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn tag_shorter_than_its_own_header_is_corrupt() {
        assert!(matches!(
            read_text(&[0x1A, 1, 0x00], 0),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn tag_past_end_of_dat1_is_corrupt() {
        assert!(matches!(
            read_text(&[0x1A, 5, 0x00], 0),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn ordered_form_and_shift_bytes_are_read() {
        let mid1 = [0, 0, 0xF3, 0x00, 0, 0, 0, 0];

        let header = read_mid1(&mid1).unwrap();
        assert!(header.ordered);
        assert_eq!(header.form, 0x3);
        assert_eq!(header.shift_bytes, 0);
    }

    #[test]
    fn nonzero_shift_bytes_is_corrupt() {
        let mid1 = [0, 0, 0x00, 0x02, 0, 0, 0, 0];
        assert!(matches!(read_mid1(&mid1), Err(Error::Corrupt(_))));
    }

    fn inf1_header(count: u16, record_len: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(count.to_be_bytes());
        out.extend(record_len.to_be_bytes());
        out.extend([0; 4]); // group id + padding, neither read
        out
    }

    #[test]
    fn messages_are_read_with_their_text_and_attributes() {
        let mut inf1 = inf1_header(2, 6);
        inf1.extend(0u32.to_be_bytes());
        inf1.extend([0xAA, 0xBB]);
        inf1.extend(3u32.to_be_bytes());
        inf1.extend([0xCC, 0xDD]);
        let dat1 = b"Hi\0Yo\0";

        let (messages, record_len, mid1_header) = read_messages(&inf1, dat1, None).unwrap();

        assert_eq!(record_len, 6);
        assert!(mid1_header.is_none());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, MessageId(0));
        assert_eq!(messages[0].public_id, 0);
        assert_eq!(messages[0].attributes, [0xAA, 0xBB]);
        assert_eq!(messages[0].text, [TextSegment::Text(b"Hi".to_vec())]);
        assert_eq!(messages[1].id, MessageId(1));
        assert_eq!(messages[1].attributes, [0xCC, 0xDD]);
        assert_eq!(messages[1].text, [TextSegment::Text(b"Yo".to_vec())]);
    }

    /// The regression this whole module change was about: an id comes out of
    /// its MID1 entry whole, not masked to the entry's low 16 bits.
    #[test]
    fn public_id_comes_from_mid1() {
        let mut inf1 = inf1_header(2, 4);
        inf1.extend(0u32.to_be_bytes());
        inf1.extend(3u32.to_be_bytes());
        let dat1 = b"Hi\0Yo\0";
        let mut mid1 = vec![0, 0, 0x00, 0x00, 0, 0, 0, 0];
        mid1.extend(5u32.to_be_bytes());
        mid1.extend(10u32.to_be_bytes());

        let (messages, _, mid1_header) = read_messages(&inf1, dat1, Some(&mid1)).unwrap();

        assert_eq!(messages[0].public_id, 5);
        assert_eq!(messages[1].public_id, 10);
        assert_eq!(mid1_header.unwrap().shift_bytes, 0);
    }

    #[test]
    fn mid1_id_too_large_is_corrupt() {
        let mut inf1 = inf1_header(1, 4);
        inf1.extend(0u32.to_be_bytes());
        let mut mid1 = vec![0, 0, 0x00, 0x00, 0, 0, 0, 0];
        mid1.extend(0x0001_0000u32.to_be_bytes());

        assert!(matches!(
            read_messages(&inf1, &[], Some(&mid1)),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn record_len_narrower_than_text_offset_is_corrupt() {
        let inf1 = inf1_header(1, 3);
        assert!(matches!(
            read_messages(&inf1, &[], None),
            Err(Error::Corrupt(_))
        ));
    }
}
