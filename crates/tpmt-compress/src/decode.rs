//! The read path: turns Yaz0 data into raw bytes.

use tpmt_bytes::Reader;

use crate::token::backref::Backreference;
use crate::token::{Flags, GROUP_SIZE, TOP_FLAG_BIT, Token};
use crate::{Error, Result, header, is_yaz0};

/// Decompresses Yaz0 data. See the crate docs for the token format.
///
/// # Errors
///
/// Returns [`Error::NotYaz0`] if `input` lacks the magic, [`Error::BackReference`]
/// if a back-reference reaches before the start of the output,
/// [`Error::SizeMismatch`] if the decoded output doesn't match the header's
/// declared size, or [`Error::Bytes`] if `input` is truncated.
pub fn yaz0_decode(input: &[u8]) -> Result<Vec<u8>> {
    // is_yaz0 gets the raw buffer: starts_with copes with one shorter than
    // the magic, and nothing else here needs to know the magic's length.
    if !is_yaz0(input) {
        return Err(Error::NotYaz0);
    }

    let mut reader = Reader::new(input);
    let decompressed_size = reader.u32_at(header::DECOMPRESSED_SIZE)? as usize;
    reader.seek(header::LEN);

    let mut out = Vec::with_capacity(decompressed_size);
    let mut flags: Flags = 0;
    let mut items_left = 0;

    while out.len() < decompressed_size {
        if items_left == 0 {
            flags = reader.u8()?;
            items_left = GROUP_SIZE;
        }
        // Peek into top bit, move along
        let is_literal = flags & TOP_FLAG_BIT != 0;
        flags <<= 1;
        items_left -= 1;

        let token = if is_literal {
            Token::Literal(reader.u8()?)
        } else {
            Token::BackReference(Backreference::read(&mut reader)?)
        };
        let Backreference { distance, length } = match token {
            Token::Literal(byte) => {
                out.push(byte);
                continue;
            }
            Token::BackReference(backref) => backref,
        };

        let start = out
            .len()
            .checked_sub(distance as usize)
            .ok_or(Error::BackReference {
                pos: out.len(),
                distance: distance as usize,
            })?;
        if distance >= length {
            // Source and destination don't overlap, so the whole run already
            // sits in `out` and can be copied in one shot.
            out.extend_from_within(start..start + length as usize);
        } else {
            // One byte at a time here: a run this short repeats a pattern
            // shorter than itself, so the bytes this copy writes are bytes
            // it then goes on to read.
            for i in 0..length {
                let byte = out[start + i as usize];
                out.push(byte);
            }
        }
    }

    if out.len() != decompressed_size {
        return Err(Error::SizeMismatch {
            expected: decompressed_size,
            actual: out.len(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A literal group, then a back-reference over the four bytes it wrote.
    fn sample() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(crate::header::MAGIC);
        data.extend_from_slice(&10u32.to_be_bytes());
        data.extend_from_slice(&[0u8; 8]);
        // Four literals, then a reference: length 4 + 2, distance 3 + 1.
        data.push(0b1111_0000);
        data.extend_from_slice(b"abcd");
        data.extend_from_slice(&[0x40, 0x03]);
        data
    }

    #[test]
    fn decodes_literals_and_overlapping_runs() {
        // The tail of the run reads back bytes the run itself just wrote.
        assert_eq!(yaz0_decode(&sample()).unwrap(), b"abcdabcdab");
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(yaz0_decode(b"RARC...."), Err(Error::NotYaz0)));
    }

    /// Truncated input is an error, never a short buffer passed off as whole.
    #[test]
    fn rejects_truncated_input() {
        let data = sample();
        assert!(yaz0_decode(&data[..data.len() - 3]).is_err());
    }

    /// A match that leaves the output short of, or past, the header's
    /// declared size is treated as corruption rather than silently accepted.
    #[test]
    fn rejects_a_match_that_misses_the_declared_size() {
        let mut data = Vec::new();
        data.extend_from_slice(crate::header::MAGIC);
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&[0u8; 8]);
        data.push(0b1000_0000);
        data.push(b'a');
        // Length (4 - 1) + 3, distance 0 + 1: writes 7 bytes total, not 3.
        data.extend_from_slice(&[0x40, 0x00]);
        assert!(matches!(
            yaz0_decode(&data),
            Err(Error::SizeMismatch { .. })
        ));
    }

    /// A reference pointing further back than the output goes is corruption,
    /// and the byte before the start of a buffer is not readable.
    #[test]
    fn rejects_a_back_reference_past_the_start() {
        let mut data = Vec::new();
        data.extend_from_slice(crate::header::MAGIC);
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend_from_slice(&[0u8; 8]);
        data.push(0b0000_0000);
        data.extend_from_slice(&[0x40, 0x03]);
        assert!(matches!(
            yaz0_decode(&data),
            Err(Error::BackReference { .. })
        ));
    }
}
