//! Message text and attributes: INF1, DAT1, and MID1.
//!
//! One message is spread across three sections: its fixed-width attribute
//! record in INF1, the text that record points at in DAT1, and, when the
//! file has one, the public-facing id sitting at the same position in MID1.

use tpmt_bytes::{Reader, Writer};

use crate::{Error, Result};

/// What opens a message's text: 0x1A, then the whole tag's length.
const TAG_OPENER: u8 = 0x1A;
/// The smallest a tag can be, the opener and the length byte.
const TAG_HEADER_LEN: usize = 2;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The id external callers look this message up by, held in MID1.
    /// A game may keep a copy in its attributes too, which this crate leaves
    /// as it finds it.
    ///
    /// Meaningless when the file has no [`crate::Bmg::mid1`], since such a
    /// file is addressed by position instead and its attributes open with
    /// whatever the game put there.
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
///
/// The header also carries an `ordered` bit, the high nibble of the byte
/// `form` is in, promising the ids are sorted so the game may binary search
/// them. It is not a field here: it is a fact about the ids, so it is worked
/// out from them on write, and on read a file that claims it of unsorted ids
/// is refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mid1Header {
    /// Which layout the id array is in, the low nibble of its byte.
    /// The game asserts this is zero and never branches on it (sanity check).
    pub form: u8,
    /// How a stored id packs a second, independently packed id below it
    /// (say, an item and a variant): 1-3 shift the primary id left that many
    /// bytes to make room, 4 hands the whole entry to the second id instead.
    /// Always zero in practice, so a nonzero value is rejected on read
    /// rather than guessed at. See `read_mid1`.
    pub shift_bytes: u8,
}

/// The 8 byte header in front of INF1's records: how many there are, and how
/// wide one is.
mod inf1_offsets {
    /// How wide the header is, so also where its records start.
    pub const LEN: usize = 0x08;
    pub const COUNT: usize = 0x00;
    pub const RECORD_LEN: usize = 0x02;
    /// Neither read nor kept. `JMessage::TResource` can branch on this (see
    /// JSystem/JMessage/resource.cpp), but TP's `dMsgObject_c` bypasses that
    /// parser and derives group purely from message id (> 5000).
    pub const _GROUP_ID: usize = 0x04;
    // 0x06, 2 bytes: padding.
}

/// The 8 byte header in front of MID1's id array.
mod mid1_offsets {
    /// How wide the header is, so also where the id array starts.
    pub const LEN: usize = 0x08;
    /// Not read: `count` is redundant with INF1's own record count, which is
    /// what the array is actually walked by. Written as that count.
    pub const COUNT: usize = 0x00;
    /// High nibble `ordered`, low nibble `form`.
    pub const ORDERED_FORM: usize = 0x02;
    pub const SHIFT_BYTES: usize = 0x03;
    // 0x04, 4 bytes: padding.
}

/// The text offset at the front of every INF1 record.
const TEXT_OFFSET_LEN: u16 = 4;

/// The messages, and how wide one INF1 record is.
///
/// All three sections are read together because one message is spread over all
/// of them: its record in INF1, the text that record points at in DAT1, and
/// the id sitting at the same position in MID1.
pub fn read(
    inf1: &[u8],
    dat1: &[u8],
    mid1: Option<&[u8]>,
) -> Result<(Vec<Message>, u16, Option<Mid1Header>)> {
    let reader = Reader::new(inf1);
    let count = reader.u16_at(inf1_offsets::COUNT)? as usize;
    // Text offset into DAT1 + attribute bytes
    let record_len = reader.u16_at(inf1_offsets::RECORD_LEN)?;
    let attributes_len = record_len
        .checked_sub(TEXT_OFFSET_LEN)
        .ok_or(Error::Corrupt(
            "an INF1 record is narrower than its own text offset",
        ))?;
    let records = reader.slice_at(inf1_offsets::LEN, count * record_len as usize)?;

    // `shift_bytes` is guaranteed zero by `read_mid1`, so a MID1 entry is
    // always the id whole; see `Mid1Header::shift_bytes`.
    let mid1 = mid1.map(Reader::new);

    let mut messages = Vec::with_capacity(count);
    // A message's id is its position, counted in the id's own width rather
    // than narrowed out of a `usize` index.
    for (id, record) in (0..).zip(records.chunks_exact(record_len as usize)) {
        let mut record = Reader::new(record);
        let dat_offset = record.u32()? as usize;
        let attributes = record.take(attributes_len as usize)?.to_vec();

        let public_id = match &mid1 {
            Some(mid1) => {
                let entry = mid1.u32_at(mid1_offsets::LEN + id as usize * 4)?;
                u16::try_from(entry)
                    .map_err(|_| Error::Corrupt("a MID1 id does not fit in 16 bits"))?
            }
            None => 0,
        };

        messages.push(Message {
            public_id,
            id: MessageId(id),
            attributes,
            text: read_text(dat1, dat_offset)?,
        });
    }

    let mid1_header = mid1.map(|mid1| read_mid1(&mid1, &messages)).transpose()?;
    Ok((messages, record_len, mid1_header))
}

/// Whether the ids are in the order MID1's `ordered` bit promises the game,
/// which binary searches them when it is set.
fn sorted(messages: &[Message]) -> bool {
    messages.is_sorted_by_key(|message| message.public_id)
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
            TAG_OPENER => {
                if i > text_start {
                    segments.push(TextSegment::Text(dat1[text_start..i].to_vec()));
                }
                let len = *dat1
                    .get(i + 1)
                    .ok_or(Error::Corrupt("a tag is cut off before its length byte"))?
                    as usize;
                if len < TAG_HEADER_LEN {
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

/// What MID1 says about its ids, as against the ids themselves, which are
/// what the `ordered` bit is checked against.
fn read_mid1(mid1: &Reader<'_>, messages: &[Message]) -> Result<Mid1Header> {
    let byte = mid1.u8_at(mid1_offsets::ORDERED_FORM)?;
    let shift_bytes = mid1.u8_at(mid1_offsets::SHIFT_BYTES)?;
    // `ordered` high nibble
    if byte & 0xF0 != 0 && !sorted(messages) {
        return Err(Error::Corrupt(
            "a MID1 header claims its ids are sorted, and they are not",
        ));
    }
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
        form: byte & 0x0F,
        shift_bytes,
    })
}

/// The three sections a message is spread across.
pub type MessageSections = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

/// INF1, DAT1, and MID1 when the file has one, back from what [`read`] took
/// apart.
///
/// DAT1 opens with one terminator on its own, then holds each message's text
/// in turn, so no message starts at offset zero. Retail choice - no other reason.
pub fn write(
    messages: &[Message],
    record_len: u16,
    mid1: Option<Mid1Header>,
) -> Result<MessageSections> {
    let attributes_len = record_len
        .checked_sub(TEXT_OFFSET_LEN)
        .ok_or(Error::Unwritable(
            "an INF1 record is narrower than its own text offset",
        ))?;

    let mut inf1 = Writer::with_capacity(inf1_offsets::LEN + messages.len() * record_len as usize);
    inf1.zeros(inf1_offsets::LEN);
    inf1.u16_at(inf1_offsets::COUNT, count(messages)?);
    inf1.u16_at(inf1_offsets::RECORD_LEN, record_len);

    let mut dat1 = Writer::new();
    dat1.u8(0);

    for message in messages {
        if message.attributes.len() != attributes_len as usize {
            return Err(Error::Unwritable(
                "a message's attributes are not the width the file states",
            ));
        }
        inf1.u32(u32::try_from(dat1.len()).map_err(|_| Error::Oversized)?);
        inf1.bytes(&message.attributes);
        write_text(&mut dat1, &message.text)?;
    }

    let mid1 = mid1
        .map(|header| write_mid1(header, messages))
        .transpose()?;
    Ok((inf1.finish(), dat1.finish(), mid1))
}

/// How many messages there are, in the width both INF1 and MID1 store it.
fn count(messages: &[Message]) -> Result<u16> {
    u16::try_from(messages.len()).map_err(|_| Error::Oversized)
}

/// One message's text, terminated. Each run is checked to be what
/// [`read_text`] would split back out of it, since a terminator or an opener
/// in the wrong place is a message that quietly ends early.
fn write_text(dat1: &mut Writer, text: &[TextSegment]) -> Result<()> {
    for segment in text {
        match segment {
            TextSegment::Text(run) => {
                if run.iter().any(|&byte| byte == 0x00 || byte == TAG_OPENER) {
                    return Err(Error::Unwritable(
                        "a text run holds a terminator or a tag opener",
                    ));
                }
                dat1.bytes(run);
            }
            TextSegment::Tag(tag) => {
                let stated = tag.get(1).map(|&len| len as usize);
                if tag.first() != Some(&TAG_OPENER) || stated != Some(tag.len()) {
                    return Err(Error::Unwritable(
                        "a tag does not open with 0x1A and its own length",
                    ));
                }
                dat1.bytes(tag);
            }
        }
    }
    dat1.u8(0);
    Ok(())
}

/// The header, then one id per message, whole. See [`read_mid1`] for why a
/// packed second id is refused rather than written.
fn write_mid1(header: Mid1Header, messages: &[Message]) -> Result<Vec<u8>> {
    if header.shift_bytes != 0 {
        return Err(Error::Unwritable(
            "a MID1 header packs a second id into the message id, which is unsupported",
        ));
    }
    if header.form > 0x0F {
        return Err(Error::Unwritable("a MID1 form does not fit its nibble"));
    }
    let mut out = Writer::with_capacity(mid1_offsets::LEN + messages.len() * 4);
    out.zeros(mid1_offsets::LEN);
    out.u16_at(mid1_offsets::COUNT, count(messages)?);
    out.u8_at(
        mid1_offsets::ORDERED_FORM,
        u8::from(sorted(messages)) << 4 | header.form,
    );
    out.u8_at(mid1_offsets::SHIFT_BYTES, header.shift_bytes);
    for message in messages {
        out.u32(message.public_id as u32);
    }
    Ok(out.finish())
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
    fn form_and_shift_bytes_are_read() {
        let mid1 = [0, 0, 0xF3, 0x00, 0, 0, 0, 0];

        let header = read_mid1(&Reader::new(&mid1), &[]).unwrap();
        assert_eq!(header.form, 0x3);
        assert_eq!(header.shift_bytes, 0);
    }

    #[test]
    fn nonzero_shift_bytes_is_corrupt() {
        let mid1 = [0, 0, 0x00, 0x02, 0, 0, 0, 0];
        assert!(matches!(
            read_mid1(&Reader::new(&mid1), &[]),
            Err(Error::Corrupt(_))
        ));
    }

    /// The game binary searches the ids when the bit is set, so a file that
    /// sets it over unsorted ids has lookups that miss.
    #[test]
    fn claiming_unsorted_ids_are_ordered_is_corrupt() {
        let unsorted = [message(0, 10, &[], &[]), message(1, 5, &[], &[])];
        let ordered = [0, 0, 0x10, 0x00, 0, 0, 0, 0];
        assert!(matches!(
            read_mid1(&Reader::new(&ordered), &unsorted),
            Err(Error::Corrupt(_))
        ));
        let unordered = [0, 0, 0x00, 0x00, 0, 0, 0, 0];
        assert!(read_mid1(&Reader::new(&unordered), &unsorted).is_ok());
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

        let (messages, record_len, mid1_header) = read(&inf1, dat1, None).unwrap();

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

    /// An id comes out of its MID1 entry whole, not masked to the entry's
    /// low 16 bits.
    #[test]
    fn public_id_comes_from_mid1() {
        let mut inf1 = inf1_header(2, 6);
        inf1.extend(0u32.to_be_bytes());
        inf1.extend(5u16.to_be_bytes());
        inf1.extend(3u32.to_be_bytes());
        inf1.extend(10u16.to_be_bytes());
        let dat1 = b"Hi\0Yo\0";
        let mut mid1 = vec![0, 0, 0x00, 0x00, 0, 0, 0, 0];
        mid1.extend(5u32.to_be_bytes());
        mid1.extend(10u32.to_be_bytes());

        let (messages, _, mid1_header) = read(&inf1, dat1, Some(&mid1)).unwrap();
        assert_eq!(messages[0].public_id, 5);
        assert_eq!(messages[1].public_id, 10);
        assert_eq!(mid1_header.unwrap().shift_bytes, 0);
    }

    /// Whether the attributes repeat the id is up to the game, so a copy
    /// that disagrees with MID1, or no room for one, reads fine.
    #[test]
    fn attributes_are_not_checked_against_mid1() {
        let mut inf1 = inf1_header(1, 6);
        inf1.extend(0u32.to_be_bytes());
        inf1.extend(6u16.to_be_bytes());
        let mut mid1 = vec![0, 0, 0x00, 0x00, 0, 0, 0, 0];
        mid1.extend(5u32.to_be_bytes());
        let (messages, _, _) = read(&inf1, b"Hi\0", Some(&mid1)).unwrap();
        assert_eq!(messages[0].public_id, 5);
        assert_eq!(messages[0].attributes, [0x00, 0x06]);

        let mut narrow = inf1_header(1, 4);
        narrow.extend(0u32.to_be_bytes());
        assert!(read(&narrow, b"Hi\0", Some(&mid1)).is_ok());
    }

    #[test]
    fn mid1_id_too_large_is_corrupt() {
        let mut inf1 = inf1_header(1, 6);
        inf1.extend(0u32.to_be_bytes());
        inf1.extend([0, 0]);
        let mut mid1 = vec![0, 0, 0x00, 0x00, 0, 0, 0, 0];
        mid1.extend(0x0001_0000u32.to_be_bytes());

        assert!(matches!(
            read(&inf1, &[], Some(&mid1)),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn record_len_narrower_than_text_offset_is_corrupt() {
        let inf1 = inf1_header(1, 3);
        assert!(matches!(read(&inf1, &[], None), Err(Error::Corrupt(_))));
    }

    fn message(id: u32, public_id: u16, attributes: &[u8], text: &[TextSegment]) -> Message {
        Message {
            public_id,
            id: MessageId(id),
            attributes: attributes.to_vec(),
            text: text.to_vec(),
        }
    }

    fn sample() -> Vec<Message> {
        vec![
            message(0, 5, &[0x00, 0x05], &[TextSegment::Text(b"Hi".to_vec())]),
            message(
                1,
                10,
                &[0x00, 0x0A],
                &[
                    TextSegment::Tag(vec![0x1A, 3, 0x01]),
                    TextSegment::Text(b"Yo".to_vec()),
                ],
            ),
        ]
    }

    const HEADER: Mid1Header = Mid1Header {
        form: 0,
        shift_bytes: 0,
    };

    /// The retail layout, field by field: DAT1 opens with a lone terminator,
    /// so the first message's text is at offset 1, and MID1 packs the
    /// `ordered` bit into the high nibble above `form`.
    #[test]
    fn messages_are_written_the_way_the_retail_files_are() {
        let (inf1, dat1, mid1) = write(&sample(), 6, Some(HEADER)).unwrap();

        let mut expected = inf1_header(2, 6);
        expected.extend(1u32.to_be_bytes());
        expected.extend([0x00, 0x05]);
        expected.extend(4u32.to_be_bytes());
        expected.extend([0x00, 0x0A]);
        assert_eq!(inf1, expected);
        assert_eq!(dat1, b"\0Hi\0\x1A\x03\x01Yo\0");
        assert_eq!(
            mid1.unwrap(),
            [0, 2, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 10]
        );
    }

    #[test]
    fn messages_survive_a_round_trip() {
        let header = Mid1Header {
            form: 3,
            shift_bytes: 0,
        };
        let (inf1, dat1, mid1) = write(&sample(), 6, Some(header)).unwrap();
        let (messages, record_len, read_header) = read(&inf1, &dat1, mid1.as_deref()).unwrap();
        assert_eq!(messages, sample());
        assert_eq!(record_len, 6);
        assert_eq!(read_header, Some(header));
    }

    #[test]
    fn attributes_are_written_as_given() {
        let stale = [message(0, 5, &[0xAA, 0xBB], &[])];
        let (inf1, _, _) = write(&stale, 6, Some(HEADER)).unwrap();
        assert_eq!(&inf1[inf1_offsets::LEN + 4..], [0xAA, 0xBB]);

        let no_room = [message(0, 5, &[], &[])];
        assert!(write(&no_room, 4, Some(HEADER)).is_ok());
    }

    /// The bit is a fact about the ids, so it follows them: sorted in, set;
    /// shuffled, clear.
    #[test]
    fn the_ordered_bit_follows_the_ids() {
        let (_, _, mid1) = write(&sample(), 6, Some(HEADER)).unwrap();
        assert_eq!(mid1.unwrap()[2], 0x10);

        let mut shuffled = sample();
        shuffled.swap(0, 1);
        let (_, _, mid1) = write(&shuffled, 6, Some(HEADER)).unwrap();
        assert_eq!(mid1.unwrap()[2], 0x00);
    }

    #[test]
    fn attributes_of_the_wrong_width_are_unwritable() {
        assert!(matches!(
            write(&sample(), 8, None),
            Err(Error::Unwritable(_))
        ));
    }

    /// Either would be read back as something else: the run would end at
    /// the terminator, and the tag would swallow whatever its length byte
    /// said rather than what it holds.
    #[test]
    fn text_the_reader_would_split_differently_is_unwritable() {
        let terminator = [message(0, 0, &[], &[TextSegment::Text(b"a\0b".to_vec())])];
        assert!(matches!(
            write(&terminator, 4, None),
            Err(Error::Unwritable(_))
        ));
        let tag = [message(0, 0, &[], &[TextSegment::Tag(vec![0x1A, 4, 0x01])])];
        assert!(matches!(write(&tag, 4, None), Err(Error::Unwritable(_))));
    }
}
