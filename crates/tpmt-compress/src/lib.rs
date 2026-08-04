//! Nintendo compression formats used by GameCube-era titles.
//!
//! Yaz0, an LZSS variant, wraps most archives on the disc and is the only
//! one this project has needed so far. Compression only, knows nothing about
//! what it is wrapping.
//!
//! Tokens come in eights behind a flag byte, one bit each: 1 for a literal
//! byte, 0 for a back-reference (a 12-bit distance and a length nibble,
//! each stored with a small offset baked in, detailed at their use).
//!
//! The output doubles as the dictionary a back-reference reads from, copied
//! one byte at a time since a run's source and destination can overlap.

// TODO: yaz0 encoding, once anything needs to put an archive back.

// TODO: Yay0 and ASR support (maybe). The engine supports both alongside Yaz0
// so they may be needed eventually.

use tpmt_bytes::Reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not Yaz0 data")]
    NotYaz0,

    #[error("a back-reference reaches {distance} bytes back from offset {pos}")]
    BackReference { pos: usize, distance: usize },

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `Yaz0`, then the decompressed size, then 8 reserved bytes.
const YAZ0_MAGIC: &[u8; 4] = b"Yaz0";
const YAZ0_HEADER_LEN: usize = 0x10;

/// Whether a buffer starts with the Yaz0 magic.
///
/// Callers decide what to do about it: on this disc the wrapper is a convention
/// of where a file sits, not something the file itself declares.
pub fn is_yaz0(data: &[u8]) -> bool {
    data.starts_with(YAZ0_MAGIC)
}

/// Decompresses a Yaz0 stream. See the module docs for the token format.
pub fn yaz0_decode(input: &[u8]) -> Result<Vec<u8>> {
    // Passed whole rather than pre-sliced via reader: starts_with handles a
    // buffer shorter than the magic without panicking, and only is_yaz0 needs
    // to know the magic's length.
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
        data.extend_from_slice(YAZ0_MAGIC);
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
        data.extend_from_slice(YAZ0_MAGIC);
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
