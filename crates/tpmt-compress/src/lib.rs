//! Nintendo compression formats used by GameCube-era titles.
//!
//! Yaz0, an LZSS variant, wraps most archives on the disc and is the only
//! one this project has needed so far. Just the codec, in both directions; it
//! knows nothing about what it is wrapping.
//!
//! Tokens come in eights behind a flag byte, one bit each: 1 for a literal
//! byte, 0 for a back-reference (a 12-bit distance and a length nibble,
//! each stored with a small offset baked in, detailed at their use).
//!
//! The output doubles as the dictionary a back-reference reads from, copied
//! one byte at a time since a run's source and destination can overlap.

// TODO: Yay0 and ASR support (maybe). The engine supports both alongside Yaz0
// so they may be needed eventually.

mod decode;
mod encode;

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

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

const YAZ0_MAGIC: &[u8; 4] = b"Yaz0";
/// The magic, the decompressed size, then 8 reserved bytes.
const YAZ0_HEADER_LEN: usize = 0x10;

/// Whether a buffer starts with the Yaz0 magic.
///
/// Callers decide what to do about it: on this disc the wrapper is a convention
/// of where a file sits, not something the file itself declares.
pub fn is_yaz0(data: &[u8]) -> bool {
    data.starts_with(YAZ0_MAGIC)
}
