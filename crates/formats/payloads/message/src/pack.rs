//! The write path: turns a [`Bmg`] back into bytes.

use tpmt_binary::{Be32, Record, Writer};

use crate::Header;
use crate::sections::{self, flow, message, positions};
use crate::{Bmg, Error, FileKind, Result};

/// Lays the sections out in the order the retail files have them.
///
/// Every section is written padded out to the size it states, and the
/// padding after the last one is trimmed off at the end, as on the discs,
/// though that section's stated size still counts it.
// similar names `flw1`/`fli1` are warrented
#[allow(clippy::similar_names)]
pub fn pack(bmg: &Bmg) -> Result<Vec<u8>> {
    let (inf1, dat1, mid1) = message::write(&bmg.messages, bmg.record_len, bmg.mid1)?;
    let mut bodies = vec![(sections::INF1, inf1), (sections::DAT1, dat1)];
    if let Some(mid1) = mid1 {
        bodies.push((sections::MID1, mid1));
    }
    if let Some(strings) = &bmg.strings {
        bodies.push((sections::STR1, write_strings(strings)?));
    }
    // The stated size stops here, whether or not a flow pair follows.
    let stated = bodies.len();
    if let Some(flow) = &bmg.flow {
        let messages = positions(
            bmg.messages.iter().map(|message| message.id),
            "two messages share an id",
        )?;
        let (flw1, fli1) = flow::write(flow, &messages)?;
        bodies.push((sections::FLW1, flw1));
        bodies.push((sections::FLI1, fli1));
    }

    let padded =
        |body: &[u8]| (sections::Header::LEN + body.len()).next_multiple_of(sections::ALIGN);
    let field = |len: usize| {
        u32::try_from(len)
            .map(Be32::new)
            .map_err(|_| Error::Oversized)
    };
    let len_of = |bodies: &[([u8; 4], Vec<u8>)]| {
        Header::LEN + bodies.iter().map(|(_, body)| padded(body)).sum::<usize>()
    };

    let mut out = Writer::with_capacity(len_of(&bodies));
    out.record(&Header {
        magic: FileKind::Mesg.magic(),
        kind: Header::KIND,
        size: field(len_of(&bodies[..stated]))?,
        section_count: field(bodies.len())?,
        encoding: bmg.encoding.byte(),
        unnamed: [0; 15],
    });
    let mut body_end = out.len();
    for (magic, body) in &bodies {
        out.record(&sections::Header {
            magic: *magic,
            size: field(padded(body))?,
        });
        out.bytes(body);
        body_end = out.len();
        out.align(sections::ALIGN);
    }

    let mut out = out.finish();
    out.truncate(body_end);
    Ok(out)
}

/// The string pool rejoined on its terminators, the inverse of
/// `unpack::read_strings`. A terminator inside an entry would come back as
/// two entries, so it is refused.
fn write_strings(strings: &[Box<[u8]>]) -> Result<Vec<u8>> {
    if strings.iter().any(|string| string.contains(&0)) {
        return Err(Error::Unwritable("a string holds a terminator"));
    }
    Ok(strings.join(&0))
}

#[cfg(test)]
mod tests {
    use tpmt_binary::{Reader, Record};

    use super::*;
    use crate::sections::Header as SectionHeader;
    use crate::sections::flow::{Node, NodeId, Root};
    use crate::{Encoding, Flow, Format, Message, MessageId, Mid1Header, TextSegment};

    fn sample() -> Bmg {
        Bmg {
            encoding: Encoding::ShiftJis,
            record_len: 6,
            mid1: Some(Mid1Header {
                form: 0,
                shift_bytes: 0,
            }),
            messages: vec![Message {
                public_id: 5,
                id: MessageId(0),
                attributes: Box::new([0, 5]),
                text: vec![TextSegment::Text(Box::new(*b"Hi"))],
            }],
            flow: Some(Flow {
                nodes: vec![Node::Text {
                    id: NodeId(0),
                    message: MessageId(0),
                    next: None,
                }],
                roots: vec![Root {
                    public_id: 3000,
                    node: NodeId(0),
                }],
            }),
            strings: None,
        }
    }

    /// The header and the section table, field by field: five sections, the
    /// stated size stopping at the flow pair, every section padded to 0x20
    /// but the last, whose stated size counts its padding regardless.
    #[test]
    fn packs_the_retail_layout() {
        let data = pack(&sample()).unwrap();
        let reader = Reader::new(&data);

        let top: &Header = reader.record_at(0).unwrap();
        assert_eq!(top.magic, FileKind::Mesg.magic());
        assert_eq!(top.kind, Header::KIND);
        assert_eq!(top.size.get(), 0x80);
        assert_eq!(top.section_count.get(), 5);
        assert_eq!(top.encoding, Encoding::ShiftJis.byte());
        assert_eq!(top.unnamed, [0; 15]);

        // One text node, padded to two records, and no table at all.
        for (at, magic) in [
            (0x20, b"INF1"),
            (0x40, b"DAT1"),
            (0x60, b"MID1"),
            (0x80, b"FLW1"),
            (0xA0, b"FLI1"),
        ] {
            let section: &SectionHeader = reader.record_at(at).unwrap();
            assert_eq!((&section.magic, section.size.get()), (magic, 0x20));
        }
        // Header, count, and one 8 byte root: the padding is left off.
        assert_eq!(data.len(), 0xA8 + 0x10);
    }

    /// With no flow pair, the stated size is the whole file, the trimmed
    /// padding of the last section included.
    #[test]
    fn the_stated_size_is_the_whole_file_without_a_flow_pair() {
        let bmg = Bmg {
            flow: None,
            ..sample()
        };
        let data = pack(&bmg).unwrap();
        let top: &Header = Reader::new(&data).record_at(0).unwrap();
        assert_eq!(top.size.get(), 0x80);
        assert_eq!(top.section_count.get(), 3);
        // MID1 unpadded: the section header, its own header, and one id.
        assert_eq!(data.len(), 0x60 + sections::Header::LEN + 8 + 4);
    }

    #[test]
    fn a_message_file_survives_a_round_trip() {
        let bmg = sample();
        assert_eq!(Bmg::decode(&pack(&bmg).unwrap()).unwrap(), bmg);

        // The pool is its bytes, padding included, so one that fills its
        // section is the one that comes back as it went in.
        let mut strings: Vec<Box<[u8]>> = vec![Box::from(*b""), Box::from(*b"arrow")];
        strings.resize(20, Box::default());
        let bmg = Bmg {
            flow: None,
            strings: Some(strings),
            ..sample()
        };
        assert_eq!(Bmg::decode(&pack(&bmg).unwrap()).unwrap(), bmg);

        // Addressed by position, so the public id has nowhere to be stored
        // and is zero either side.
        let mut bmg = Bmg {
            mid1: None,
            ..sample()
        };
        bmg.messages[0].public_id = 0;
        assert_eq!(Bmg::decode(&pack(&bmg).unwrap()).unwrap(), bmg);
    }

    #[test]
    fn a_string_holding_a_terminator_is_unwritable() {
        let bmg = Bmg {
            strings: Some(vec![Box::from(*b"a\0b")]),
            ..sample()
        };
        assert!(matches!(pack(&bmg), Err(Error::Unwritable(_))));
    }
}
