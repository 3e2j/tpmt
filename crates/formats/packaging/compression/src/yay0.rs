//! Yay0: the same tokens as Yaz0, split across three streams instead of
//! interleaved.
//!
//! The header gives where each stream starts. The masks come first, as
//! 32-bit words read top bit first: 1 for a literal, 0 for a back-reference.
//! The links hold each back-reference's pair. The chunks hold each literal
//! byte, and each back-reference's extra length byte when its nibble is zero.
//!
//! Decoded the way `JKRDecomp::decodeSZP` does.

// TODO: once there is an app, offer Yay0 only as an advanced option, with a
// warning that some games and emulators can't read it and may crash or hang,
// because some loaders read only part of a file.

use std::borrow::Cow;

use tpmt_binary::{Be32, FileKind, Format, Reader, Record, Writer};

use crate::search::Tokens;
use crate::token::Token;
use crate::token::backref::Backreference;
use crate::{Error, Result, Strategy, size_of};

/// One mask word: one bit per token.
type Mask = u32;
/// Tokens per mask word.
const MASK_SIZE: u32 = Mask::BITS;
/// Whether the token about to be read is a literal or a back-reference.
const TOP_MASK_BIT: Mask = 1 << (Mask::BITS - 1);

/// A Yay0 wrapper, held unwrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Yay0<'a> {
    /// What the wrapper holds, decompressed. Decoding owns it; encoding can
    /// borrow the caller's buffer.
    pub data: Cow<'a, [u8]>,
    /// How [`encode`](Format::encode) searches for back-references.
    /// Decoding sets [`Strategy::Parity`].
    pub strategy: Strategy,
}

impl<'a> Format<'a> for Yay0<'a> {
    const KIND: FileKind = FileKind::Yay0;

    type Error = Error;

    /// Decompresses. See the module docs for the three streams.
    ///
    /// # Errors
    ///
    /// - [`Error::WrongKind`]
    /// - [`Error::BackReference`] if a back-reference reaches before the
    ///   start of the output.
    /// - [`Error::SizeMismatch`] if the output doesn't match the header's
    ///   declared size.
    /// - [`Error::Bytes`] if a stream is truncated.
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
        /// Always [`FileKind::Yay0`](tpmt_binary::FileKind::Yay0)'s magic.
        magic: [u8; 4],
        decompressed_size: tpmt_binary::Be32,
        /// Where the links start, from the start of the file. The masks run
        /// from the end of this header up to here.
        links: tpmt_binary::Be32,
        /// Where the chunks start, from the start of the file.
        chunks: tpmt_binary::Be32,
    }
}

/// Decompresses `input`, whose magic the caller has already checked.
fn decompress(input: &[u8]) -> Result<Vec<u8>> {
    let header: &Header = Reader::new(input).record_at(0)?;
    let decompressed_size = header.decompressed_size.get() as usize;
    let at = |pos: usize| {
        let mut reader = Reader::new(input);
        reader.seek(pos);
        reader
    };
    let mut masks = at(Header::LEN);
    let mut links = at(header.links.get() as usize);
    let mut chunks = at(header.chunks.get() as usize);

    let mut out = vec![0; decompressed_size];
    let mut pos = 0;
    let mut mask: Mask = 0;
    let mut bits_left = 0;

    while pos < decompressed_size {
        if bits_left == 0 {
            mask = masks.u32()?;
            bits_left = MASK_SIZE;
        }
        let is_literal = mask & TOP_MASK_BIT != 0;
        mask <<= 1;
        bits_left -= 1;

        if is_literal {
            out[pos] = chunks.u8()?;
            pos += 1;
            continue;
        }

        let pair = links.u16()?;
        let backref = Backreference::from_pair(pair, || Ok(chunks.u8()?))?;
        pos = backref.copy(&mut out, pos)?;
    }

    Ok(out)
}

/// Compresses `input` into Yay0 data.
fn compress(input: &[u8], strategy: Strategy) -> Result<Vec<u8>> {
    let decompressed_size = size_of(input)?;

    let mut masks = Vec::new();
    let mut links = Vec::new();
    let mut chunks = Vec::new();
    let mut mask: Mask = 0;
    let mut bits = 0;

    for token in Tokens::new(input, decompressed_size, strategy) {
        match token {
            Token::Literal(byte) => {
                mask |= TOP_MASK_BIT >> bits;
                chunks.push(byte);
            }
            Token::BackReference(matched) => {
                links.extend_from_slice(&matched.pair().to_be_bytes());
                chunks.extend(matched.extended());
            }
        }

        bits += 1;
        if bits == MASK_SIZE {
            masks.extend_from_slice(&mask.to_be_bytes());
            mask = 0;
            bits = 0;
        }
    }
    if bits > 0 {
        masks.extend_from_slice(&mask.to_be_bytes());
    }

    let links_at = Header::LEN + masks.len();
    let chunks_at = links_at + links.len();
    let offset = |pos: usize| {
        u32::try_from(pos)
            .map(Be32::new)
            .map_err(|_| Error::TooLarge { len: pos })
    };

    let mut out = Writer::with_capacity(chunks_at + chunks.len());
    out.record(&Header {
        magic: FileKind::Yay0.magic(),
        decompressed_size: Be32::new(decompressed_size),
        links: offset(links_at)?,
        chunks: offset(chunks_at)?,
    });
    let mut out = out.finish();
    out.append(&mut masks);
    out.append(&mut links);
    out.append(&mut chunks);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four literals, then a back-reference over them: length 4 + 2,
    /// distance 3 + 1.
    fn sample() -> Vec<u8> {
        let mut data = Header {
            magic: FileKind::Yay0.magic(),
            decompressed_size: Be32::new(10),
            links: Be32::new(20),
            chunks: Be32::new(22),
        }
        .as_bytes()
        .to_vec();
        data.extend_from_slice(&0xF000_0000u32.to_be_bytes());
        data.extend_from_slice(&[0x40, 0x03]);
        data.extend_from_slice(b"abcd");
        data
    }

    #[test]
    fn decodes_from_three_streams() {
        assert_eq!(*Yay0::decode(&sample()).unwrap().data, *b"abcdabcdab");
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(
            Yay0::decode(b"Yaz0...."),
            Err(Error::WrongKind(_))
        ));
    }

    /// A stream offset past the end is truncation, not an empty stream.
    #[test]
    fn rejects_a_stream_past_the_end() {
        let mut data = sample();
        data[15] = 0xFF;
        assert!(matches!(Yay0::decode(&data), Err(Error::Bytes(_))));
    }
}
