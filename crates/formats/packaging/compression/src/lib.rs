//! Nintendo compression formats used by GameCube-era titles.
//!
//! Yaz0, which wraps most archives on the disc, and Yay0 which is unused.
//!
//! To put simply, instead of storing duplicate bytes on disc, we store a small back-reference
//! to a group of previously written bytes (tokens) rather than writing verbatim. Thats it.
//!
//! Both formats share the tokens: a literal byte, or a back-reference (a
//! 12-bit distance and a length nibble). They differ only in where each token
//! goes: Yaz0 interleaves them behind flag bytes ([`yaz0`]), and Yay0 splits
//! them into three streams ([`yay0`]). So the match search is shared too.
//!
//! A loader that reads only part of a file can't read Yay0. Its three streams
//! are decoded together, so it needs the whole file at once, where Yaz0 decodes
//! front to back. Otherwise they're equal. A file encodes to the same tokens in
//! both and comes out within 3 bytes.
//!
//! The output doubles as the dictionary a back-reference reads from, copied
//! one byte at a time since a run's source and destination can overlap.

mod search;
mod token;
pub mod yay0;
pub mod yaz0;

pub use tpmt_binary::{Compression, FileKind, Format};
pub use yay0::Yay0;
pub use yaz0::Yaz0;

/// How the encoder searches for back-references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Nintendo's own search, so a retail file comes back byte for byte.
    Parity,
    /// Chases longer back-references for a smaller file. Slower, and no
    /// longer byte for byte with retail.
    Extensive,
}

/// Unwraps `data`, which opens with `compression`'s magic.
///
/// # Errors
///
/// Whatever that format's [`decode`](Format::decode) returns.
pub fn decompress(compression: Compression, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match compression {
        Compression::Yaz0 => Yaz0::decode(data)?.data,
        Compression::Yay0 => Yay0::decode(data)?.data,
    }
    .into_owned())
}

/// Wraps `data` in `compression`.
///
/// # Errors
///
/// [`Error::TooLarge`].
pub fn compress(compression: Compression, data: &[u8], strategy: Strategy) -> Result<Vec<u8>> {
    let data = std::borrow::Cow::Borrowed(data);
    match compression {
        Compression::Yaz0 => Yaz0 { data, strategy }.encode(),
        Compression::Yay0 => Yay0 { data, strategy }.encode(),
    }
}

/// `input`'s length, for a header's 32-bit size.
fn size_of(input: &[u8]) -> Result<u32> {
    u32::try_from(input.len()).map_err(|_| Error::TooLarge { len: input.len() })
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    WrongKind(#[from] tpmt_binary::WrongKind),

    #[error("a back-reference reaches {distance} bytes back from offset {pos}")]
    BackReference { pos: usize, distance: usize },

    #[error("{len} bytes does not fit in a 32-bit compression header")]
    TooLarge { len: usize },

    #[error("decoded output is {actual} bytes, but the header declares {expected}")]
    SizeMismatch { expected: usize, actual: usize },

    #[error(transparent)]
    Bytes(#[from] tpmt_binary::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::backref::{MAX_LENGTH, MIN_LENGTH};

    const ALL: [Compression; 2] = [Compression::Yaz0, Compression::Yay0];

    /// Deterministic noise, so a failure repeats.
    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x1234_5678u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                // Intentional truncation: taking the PRNG's middle byte, not narrowing a value.
                #[allow(clippy::cast_possible_truncation)]
                {
                    (state >> 16) as u8
                }
            })
            .collect()
    }

    fn round_trip(input: &[u8]) {
        for compression in ALL {
            for strategy in [Strategy::Parity, Strategy::Extensive] {
                let encoded = compress(compression, input, strategy).unwrap();
                assert_eq!(
                    Compression::of(&encoded),
                    Some(compression),
                    "encoder wrote something else entirely, {compression:?} {strategy:?}"
                );
                assert_eq!(
                    decompress(compression, &encoded).unwrap(),
                    input,
                    "on {} bytes, {compression:?} {strategy:?}",
                    input.len()
                );
            }
        }
    }

    /// Literals, back-references, and a run overlapping its own output, which
    /// is why the decoder copies a byte at a time.
    #[test]
    fn round_trips_literals_and_runs() {
        let mut input = b"the quick brown fox jumps over the quick brown dog".to_vec();
        input.extend(std::iter::repeat_n(b'!', 300));
        round_trip(&input);
    }

    /// Both length encodings and the boundary where the nibble runs out.
    #[test]
    fn round_trips_every_match_length() {
        for length in MIN_LENGTH as usize..=MAX_LENGTH as usize + 8 {
            let mut input = noise(length);
            input.extend_from_within(..);
            round_trip(&input);
        }
    }

    /// Lengths either side of a full Yaz0 group of eight and a Yay0 mask word
    /// of 32, empty included. Noise is all literals, one token per byte.
    #[test]
    fn round_trips_short_buffers() {
        for length in 0..72 {
            round_trip(&b"abc".repeat(24)[..length]);
            round_trip(&noise(length));
        }
    }
}
