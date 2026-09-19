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

pub use decode::yaz0_decode;
pub use encode::yaz0_encode;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not Yaz0 data")]
    NotYaz0,

    #[error("a back-reference reaches {distance} bytes back from offset {pos}")]
    BackReference { pos: usize, distance: usize },

    #[error("{len} bytes does not fit the 32-bit size in a Yaz0 header")]
    TooLarge { len: usize },

    #[error("decoded output is {actual} bytes, but the header declares {expected}")]
    SizeMismatch { expected: usize, actual: usize },

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

mod header {
    pub const LEN: usize = 0x10;
    pub const MAGIC: &[u8; 4] = b"Yaz0"; // at 0x00
    pub const DECOMPRESSED_SIZE: usize = 0x04;
    // 0x08 to 0x10 padding bytes
}

/// Whether a buffer starts with the Yaz0 magic.
///
/// Callers decide what to do about it: on this disc the wrapper is a convention
/// of where a file sits, not something the file itself declares.
#[must_use]
pub fn is_yaz0(data: &[u8]) -> bool {
    data.starts_with(header::MAGIC)
}
