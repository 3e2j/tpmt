//! Nintendo compression formats used by GameCube-era titles.
//!
//! Yaz0, an LZSS variant, wraps most archives on the disc.
//! Just the codec, in both directions; it knows nothing about what it is wrapping.
//!
//! To put simply, instead of storing duplicate bytes on disc, we store a small back-reference
//! to a group of previously written bytes (tokens) rather than writing verbatim. Thats it.
//!
//! Each group of up to 8 tokens is preceded by a flag byte (1 bit each) which marks which
//! of the following tokens is either a literal, or a backreference. 1 for a literal byte,
//! 0 for a back-reference (a 12-bit distance and a length nibble).
//!
//! The output doubles as the dictionary a back-reference reads from, copied
//! one byte at a time since a run's source and destination can overlap.

mod decode;
mod encode;
mod token;

use std::borrow::Cow;

pub use tpmt_binary::{FileKind, Format};

/// How the encoder searches for back-references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Nintendo's own search, so a retail file comes back byte for byte.
    Parity,
    /// Chases longer back-references for a smaller file. Slower, and no
    /// longer byte for byte with retail.
    Extensive,
}

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

    /// Decompresses. See the crate docs for the token format.
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
            data: Cow::Owned(decode::decompress(data.bytes())?),
            strategy: Strategy::Parity,
        })
    }

    /// Compresses [`data`](Self::data) with [`strategy`](Self::strategy).
    ///
    /// # Errors
    ///
    /// [`Error::TooLarge`].
    fn encode(&self) -> Result<Vec<u8>> {
        encode::compress(&self.data, self.strategy)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    WrongKind(#[from] tpmt_binary::WrongKind),

    #[error("a back-reference reaches {distance} bytes back from offset {pos}")]
    BackReference { pos: usize, distance: usize },

    #[error("{len} bytes does not fit the 32-bit size in a Yaz0 header")]
    TooLarge { len: usize },

    #[error("decoded output is {actual} bytes, but the header declares {expected}")]
    SizeMismatch { expected: usize, actual: usize },

    #[error(transparent)]
    Bytes(#[from] tpmt_binary::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

tpmt_binary::layout! {
    struct Header {
        /// Always [`FileKind::Yaz0`](tpmt_binary::FileKind::Yaz0)'s magic.
        magic: [u8; 4],
        decompressed_size: tpmt_binary::Be32,
        /// Padding, and zero.
        unnamed: [u8; 8],
    }
}
