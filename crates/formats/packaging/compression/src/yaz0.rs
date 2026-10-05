//! Yaz0: tokens in groups of eight, each group led by a flag byte.
//!
//! Each flag byte holds one bit per following token, top bit first: 1 for a
//! literal byte, 0 for a back-reference (a 12-bit distance and a length
//! nibble, plus an extra length byte when the nibble is zero).

use std::borrow::Cow;

use tpmt_binary::{Be32, FileKind, Format, Reader, Record, Writer};

use crate::search::Tokens;
use crate::token::Token;
use crate::token::backref::Backreference;
use crate::{Error, Result, Strategy, size_of};

/// The flag byte itself: one bit per token in its group.
type Flags = u8;
/// Number of tokens preceded by one flag byte, each bit marks each following [`Token`] type
// `Flags` is a `u8`, so `BITS` is always 8: this never truncates.
#[allow(clippy::cast_possible_truncation)]
const GROUP_SIZE: u8 = Flags::BITS as u8;
/// Whether the token about to be read is a literal or a match (backref).
const TOP_FLAG_BIT: Flags = 1 << (Flags::BITS - 1);

/// A Yaz0 wrapper, held unwrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yaz0<'a> {
    /// What the wrapper holds, decompressed. Decoding owns it; encoding can
    /// borrow the caller's buffer.
    pub data: Cow<'a, [u8]>,
    /// How [`encode`](Format::encode) searches for back-references.
    /// Decoding sets [`Strategy::Parity`].
    pub strategy: Strategy,
}

impl<'a> Format<'a> for Yaz0<'a> {
    const KIND: FileKind = FileKind::Yaz0;

    type Error = Error;

    /// Decompresses. See the module docs for the token format.
    ///
    /// # Errors
    ///
    /// - [`Error::WrongKind`]
    /// - [`Error::BackReference`] if a back-reference reaches before the
    ///   start of the output.
    /// - [`Error::SizeMismatch`] if the output doesn't match the header's
    ///   declared size.
    /// - [`Error::Bytes`] if the data is truncated.
    fn decode_body(data: tpmt_binary::Checked<'a>) -> Result<Self> {
        Ok(Self {
            data: Cow::Owned(decompress(data.bytes())?),
            strategy: Strategy::Parity,
        })
    }

    /// Compresses [`data`](Self::data) with [`strategy`](Self::strategy).
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`].
    fn encode(&self) -> Result<Vec<u8>> {
        compress(&self.data, self.strategy)
    }
}

tpmt_binary::record! {
    struct Header {
        /// Always [`FileKind::Yaz0`](tpmt_binary::FileKind::Yaz0)'s magic.
        magic: [u8; 4],
        decompressed_size: tpmt_binary::Be32,
        /// Padding, and zero.
        unnamed: [u8; 8],
    }
}

/// Decompresses `input`, whose magic the caller has already checked.
fn decompress(input: &[u8]) -> Result<Vec<u8>> {
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

        let pair = reader.u16()?;
        let backref = Backreference::from_pair(pair, || Ok(reader.u8()?))?;
        pos = backref.copy(&mut out, pos)?;
    }

    Ok(out)
}

/// Compresses `input` into Yaz0 data.
fn compress(input: &[u8], strategy: Strategy) -> Result<Vec<u8>> {
    let decompressed_size = size_of(input)?;

    let mut out = Writer::with_capacity(Header::LEN + input.len());
    out.record(&Header {
        magic: FileKind::Yaz0.magic(),
        decompressed_size: Be32::new(decompressed_size),
        unnamed: [0; 8],
    });
    let mut out = out.finish();

    let mut flags: Flags = 0;
    let mut body = Vec::new();
    let mut items = 0;

    for token in Tokens::new(input, decompressed_size, strategy) {
        match token {
            Token::Literal(byte) => {
                flags |= TOP_FLAG_BIT >> items;
                body.push(byte);
            }
            Token::BackReference(matched) => {
                body.extend_from_slice(&matched.pair().to_be_bytes());
                body.extend(matched.extended());
            }
        }

        items += 1;
        if items == GROUP_SIZE {
            out.push(flags);
            out.append(&mut body);
            flags = 0;
            items = 0;
        }
    }

    // Flushes the un-full final group.
    // As a quirk, on an exact multiple of 8, flags/body are already blank here
    // (from the reset above), but not empty, so this writes a trailing zero byte.
    // This is never read back by the decoder, but kept for Nintendo parity.
    if !input.is_empty() {
        out.push(flags);
        out.append(&mut body);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
