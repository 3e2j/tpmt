//! The write path: turns a [`Bmg`] back into bytes.

use tpmt_bytes::Writer;

use crate::sections::{self, flow, message, positions};
use crate::{Bmg, Encoding, Error, Result, header};

/// Lays the sections out in the order the retail files have them.
// similar names `flw1`/`fli1` are warrented
#[allow(clippy::similar_names)]
pub fn pack(bmg: &Bmg) -> Result<Vec<u8>> {
    let mut file = File::new(bmg.encoding);

    let (inf1, dat1, mid1) = message::write(&bmg.messages, bmg.record_len, bmg.mid1)?;
    file.section(sections::INF1, &inf1)?;
    file.section(sections::DAT1, &dat1)?;
    if let Some(mid1) = mid1 {
        file.section(sections::MID1, &mid1)?;
    }
    if let Some(strings) = &bmg.strings {
        file.section(sections::STR1, &write_strings(strings)?)?;
    }
    // The stated size stops here, whether or not a flow pair follows.
    let stated = file.len();
    if let Some(flow) = &bmg.flow {
        let messages = positions(
            bmg.messages.iter().map(|message| message.id),
            "two messages share an id",
        )?;
        let (flw1, fli1) = flow::write(flow, &messages)?;
        file.section(sections::FLW1, &flw1)?;
        file.section(sections::FLI1, &fli1)?;
    }

    file.finish(stated)
}

/// A file being laid out: the header, then one section after another.
///
/// Every section is written padded out to the size it states, and the
/// padding after the last one is trimmed off at the end, as on the discs,
/// though that section's stated size still counts it.
struct File {
    out: Writer,
    /// How many sections have been written, for the header's count field.
    section_count: usize,
    /// Where the latest section's body ends, before its padding: where the
    /// file is cut off once the last section is in.
    body_end: usize,
}

impl File {
    fn new(encoding: Encoding) -> Self {
        let mut out = Writer::new();
        out.bytes(header::MAGIC);
        out.zeros(header::LEN - header::MAGIC.len());
        out.u8_at(header::ENCODING, encoding.byte());
        Self {
            out,
            section_count: 0,
            body_end: header::LEN,
        }
    }

    const fn len(&self) -> usize {
        self.out.len()
    }

    fn section(&mut self, magic: [u8; 4], body: &[u8]) -> Result<()> {
        let size = (sections::HEADER_LEN + body.len()).next_multiple_of(sections::ALIGN);
        self.out.bytes(&magic);
        self.out
            .u32(u32::try_from(size).map_err(|_| Error::Oversized)?);
        self.out.bytes(body);
        self.body_end = self.out.len();
        self.out.align(sections::ALIGN);
        self.section_count += 1;
        Ok(())
    }

    fn finish(mut self, stated: usize) -> Result<Vec<u8>> {
        self.out.u32_at(
            header::SECTION_COUNT,
            u32::try_from(self.section_count).map_err(|_| Error::Oversized)?,
        );
        self.out.u32_at(
            header::SIZE,
            u32::try_from(stated).map_err(|_| Error::Oversized)?,
        );
        let mut out = self.out.finish();
        out.truncate(self.body_end);
        Ok(out)
    }
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
    use std::io::Read;

    use super::*;
    use crate::sections::flow::{Node, NodeId, Root};
    use crate::{Flow, Format, Message, MessageId, Mid1Header, TextSegment};

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
        assert_eq!(data.len(), 0x60 + sections::HEADER_LEN + 8 + 4);
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

    /// A retail file, byte for byte, when there is one to hand. The fixture
    /// is local game data rather than part of the repository, so its absence
    /// is a skip, not a failure.
    #[test]
    fn a_retail_file_comes_back_byte_for_byte() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/fixtures/zel_03.bmg");
        let Ok(mut file) = std::fs::File::open(path) else {
            eprintln!("skipped: no retail fixture at {path}");
            return;
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data).unwrap();

        let bmg = Bmg::decode(&data).unwrap();
        assert!(bmg.flow.is_some());
        assert_eq!(bmg.encode().unwrap(), data);
    }
}
