//! The read path: turns a Yaz0 stream into raw bytes.

use tpmt_bytes::Reader;

use crate::{Error, Result, YAZ0_HEADER_LEN, is_yaz0};

/// Decompresses a Yaz0 stream. See the crate docs for the token format.
pub fn yaz0_decode(input: &[u8]) -> Result<Vec<u8>> {
    // is_yaz0 gets the raw buffer: starts_with copes with one shorter than
    // the magic, and nothing else here needs to know the magic's length.
    if !is_yaz0(input) {
        return Err(Error::NotYaz0);
    }

    let mut reader = Reader::new(input);
    let size = reader.u32_at(4)? as usize;
    reader.seek(YAZ0_HEADER_LEN);

    let mut out = Vec::with_capacity(size);
    let mut code = 0u8;
    let mut items_left = 0;

    while out.len() < size {
        if items_left == 0 {
            code = reader.u8()?;
            items_left = 8;
        }
        let is_literal = code & 0x80 != 0;
        code <<= 1;
        items_left -= 1;

        if is_literal {
            out.push(reader.u8()?);
            continue;
        }

        let pair = reader.u16()?;
        // The stored distance is one short of the real one, so a distance field
        // of zero still means "the byte before this one".
        let distance = (pair as usize & 0x0FFF) + 1;
        let length = match pair >> 12 {
            0 => reader.u8()? as usize + 0x12,
            packed => packed as usize + 2,
        };

        let start = out
            .len()
            .checked_sub(distance)
            .ok_or(Error::BackReference {
                pos: out.len(),
                distance,
            })?;
        // One byte at a time on purpose: a run is encoded as a reference that
        // overlaps its own output, so the bytes this copy writes are bytes it
        // then goes on to read.
        for i in 0..length {
            let byte = out[start + i];
            out.push(byte);
        }
    }

    // A back-reference at the very end may write past the declared size. The
    // header is what says how long the file is, so the overshoot is spare.
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A literal group, then a back-reference over the four bytes it wrote.
    fn sample() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(crate::YAZ0_MAGIC);
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

    /// A truncated stream is an error, never a short buffer passed off as whole.
    #[test]
    fn rejects_a_truncated_stream() {
        let data = sample();
        assert!(yaz0_decode(&data[..data.len() - 3]).is_err());
    }

    /// A reference pointing further back than the output goes is corruption,
    /// and the byte before the start of a buffer is not readable.
    #[test]
    fn rejects_a_back_reference_past_the_start() {
        let mut data = Vec::new();
        data.extend_from_slice(crate::YAZ0_MAGIC);
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
