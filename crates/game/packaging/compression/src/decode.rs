//! The read path: turns Yaz0 data into raw bytes.

use crate::Header;
use crate::token::backref::Backreference;
use crate::token::{Flags, GROUP_SIZE, TOP_FLAG_BIT};
use crate::{Error, Result};
use tpmt_binary::{Reader, Record};

/// Decompresses `input`, whose magic the caller has already checked.
pub fn decompress(input: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Reader::new(input);
    let header: &Header = reader.record_at(0)?;
    let decompressed_size = header.decompressed_size.get() as usize;
    reader.seek(Header::LEN);

    let mut out = vec![0; decompressed_size];
    let mut pos = 0;
    let mut flags: Flags = 0;
    let mut items_left = 0;

    while pos < decompressed_size {
        if items_left == 0 {
            flags = reader.u8()?;
            items_left = GROUP_SIZE;
        }
        // Peek into top bit, move along
        let is_literal = flags & TOP_FLAG_BIT != 0;
        flags <<= 1;
        items_left -= 1;

        if is_literal {
            out[pos] = reader.u8()?;
            pos += 1;
            continue;
        }

        let backref = Backreference::read(&mut reader)?;
        let (distance, length) = (backref.distance() as usize, backref.length() as usize);
        let start = pos
            .checked_sub(distance)
            .ok_or(Error::BackReference { pos, distance })?;
        let end = pos + length;
        if end > decompressed_size {
            return Err(Error::SizeMismatch {
                expected: decompressed_size,
                actual: end,
            });
        }

        // A run longer than its distance repeats the bytes behind it, so each
        // pass can copy everything written since `start`, doubling the chunk.
        while pos < end {
            let chunk = (pos - start).min(end - pos);
            out.copy_within(start..start + chunk, pos);
            pos += chunk;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use tpmt_binary::Be32;

    use super::*;
    use crate::{FileKind, Format, Yaz0};

    fn header(decompressed_size: u32) -> Vec<u8> {
        Header {
            magic: FileKind::Yaz0.magic(),
            decompressed_size: Be32::new(decompressed_size),
            unnamed: [0; 8],
        }
        .as_bytes()
        .to_vec()
    }

    /// A literal group, then a back-reference over the four bytes it wrote.
    fn sample() -> Vec<u8> {
        let mut data = header(10);
        // Four literals, then a reference: length 4 + 2, distance 3 + 1.
        data.push(0b1111_0000);
        data.extend_from_slice(b"abcd");
        data.extend_from_slice(&[0x40, 0x03]);
        data
    }

    #[test]
    fn decodes_literals_and_overlapping_runs() {
        // The tail of the run reads back bytes the run itself just wrote.
        assert_eq!(*Yaz0::decode(&sample()).unwrap().data, *b"abcdabcdab");
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(
            Yaz0::decode(b"RARC...."),
            Err(Error::WrongKind(_))
        ));
    }

    /// Truncated input is an error, never a short buffer passed off as whole.
    #[test]
    fn rejects_truncated_input() {
        let data = sample();
        assert!(Yaz0::decode(&data[..data.len() - 3]).is_err());
    }

    /// A match that leaves the output short of, or past, the header's
    /// declared size is treated as corruption rather than silently accepted.
    #[test]
    fn rejects_a_match_that_misses_the_declared_size() {
        let mut data = header(3);
        data.push(0b1000_0000);
        data.push(b'a');
        // Length (4 - 1) + 3, distance 0 + 1: writes 7 bytes total, not 3.
        data.extend_from_slice(&[0x40, 0x00]);
        assert!(matches!(
            Yaz0::decode(&data),
            Err(Error::SizeMismatch { .. })
        ));
    }

    /// A reference pointing further back than the output goes is corruption,
    /// and the byte before the start of a buffer is not readable.
    #[test]
    fn rejects_a_back_reference_past_the_start() {
        let mut data = header(4);
        data.push(0b0000_0000);
        data.extend_from_slice(&[0x40, 0x03]);
        assert!(matches!(
            Yaz0::decode(&data),
            Err(Error::BackReference { .. })
        ));
    }
}
