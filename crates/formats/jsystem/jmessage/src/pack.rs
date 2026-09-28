//! The write path: turns a [`Bmg`] back into bytes.

use tpmt_bytes::{Be32, Writer};

use crate::header::{self, Header};
use crate::sections::{self, flow, message, positions};
use crate::{Bmg, Error, Result};

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
        |body: &[u8]| (sections::header::LEN + body.len()).next_multiple_of(sections::ALIGN);
    let field = |len: usize| {
        u32::try_from(len)
            .map(Be32::new)
            .map_err(|_| Error::Oversized)
    };
    let len_of = |bodies: &[([u8; 4], Vec<u8>)]| {
        header::LEN + bodies.iter().map(|(_, body)| padded(body)).sum::<usize>()
    };

    let mut out = Writer::with_capacity(len_of(&bodies));
    out.record(&Header {
        magic: header::MAGIC_FIELD,
        size: field(len_of(&bodies[..stated]))?,
        section_count: field(bodies.len())?,
        encoding: bmg.encoding.byte(),
        unnamed: [0; 15],
    });
    let mut body_end = out.len();
    for (magic, body) in &bodies {
        out.record(&sections::header::Header {
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
fn write_strings(strings: &[Vec<u8>]) -> Result<Vec<u8>> {
    if strings.iter().any(|string| string.contains(&0)) {
        return Err(Error::Unwritable("a string holds a terminator"));
    }
    Ok(strings.join(&0))
}

#[cfg(test)]
mod tests {
    use super::*;
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
                attributes: vec![0, 5],
                text: vec![TextSegment::Text(b"Hi".to_vec())],
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

        let mut expected = b"MESGbmg1".to_vec();
        expected.extend(0x80u32.to_be_bytes());
        expected.extend(5u32.to_be_bytes());
        expected.push(0x03);
        expected.resize(0x20, 0);
        assert_eq!(&data[..0x20], expected);

        assert_eq!(&data[0x20..0x24], b"INF1");
        assert_eq!(&data[0x24..0x28], 0x20u32.to_be_bytes());
        assert_eq!(&data[0x40..0x44], b"DAT1");
        assert_eq!(&data[0x44..0x48], 0x20u32.to_be_bytes());
        assert_eq!(&data[0x60..0x64], b"MID1");
        assert_eq!(&data[0x64..0x68], 0x20u32.to_be_bytes());
        // One text node, padded to two records, and no table at all.
        assert_eq!(&data[0x80..0x84], b"FLW1");
        assert_eq!(&data[0x84..0x88], 0x20u32.to_be_bytes());
        assert_eq!(&data[0xA0..0xA4], b"FLI1");
        assert_eq!(&data[0xA4..0xA8], 0x20u32.to_be_bytes());
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
        assert_eq!(&data[0x08..0x0C], 0x80u32.to_be_bytes());
        assert_eq!(&data[0x0C..0x10], 3u32.to_be_bytes());
        // MID1 unpadded: the section header, its own header, and one id.
        assert_eq!(data.len(), 0x60 + sections::header::LEN + 8 + 4);
    }

    #[test]
    fn a_message_file_survives_a_round_trip() {
        let bmg = sample();
        assert_eq!(Bmg::decode(&pack(&bmg).unwrap()).unwrap(), bmg);

        // The pool is its bytes, padding included, so one that fills its
        // section is the one that comes back as it went in.
        let mut strings = vec![b"".to_vec(), b"arrow".to_vec()];
        strings.resize(20, Vec::new());
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
            strings: Some(vec![b"a\0b".to_vec()]),
            ..sample()
        };
        assert!(matches!(pack(&bmg), Err(Error::Unwritable(_))));
    }
}
