//! The shape every file format shares: bytes in (decode), a struct out,
//! and back again (encode).
//!
//! Each format crate implements [`Format`] on its decoded struct, so
//! `Archive::decode(bytes)` and `archive.encode()` read the same everywhere.
//! Knows nothing about projects or pipelines.
//!
//! Formats are true to the file, not to the game's logic, so they hold
//! whatever a mod puts in them. A mod that changes the game past what a
//! format can express is outside what these crates can support.

use crate::{FileKind, WrongKind};

/// A file format: bytes in, `Self` out, and back again.
///
/// `'a` is the input's lifetime, for a decoded form that borrows from it
/// (an archive keeps a slice into every member). One that copies everything
/// out implements `Format<'_>`.
pub trait Format<'a>: Sized {
    /// Which kind this is, and so which magic the file opens with.
    const KIND: FileKind;

    type Error: std::error::Error + From<WrongKind>;

    /// Takes the file apart.
    ///
    /// # Errors
    ///
    /// [`WrongKind`] when `data` doesn't open with this format's magic, and
    /// whatever [`decode_body`](Self::decode_body) returns otherwise.
    fn decode(data: &'a [u8]) -> Result<Self, Self::Error> {
        Self::KIND.check(data)?;
        Self::decode_body(Checked(data))
    }

    /// Takes apart a file [`decode`](Self::decode) has already matched to
    /// this format, so an error out of it always means "this format, but
    /// broken", never "not this format". Only `decode` can make a
    /// [`Checked`], so nothing else calls this.
    ///
    /// # Errors
    ///
    /// When the file is broken. Each format's own error type says how.
    fn decode_body(data: Checked<'a>) -> Result<Self, Self::Error>;

    /// Writes the file back out.
    ///
    /// # Errors
    ///
    /// When the value doesn't fit the format: a size field overflows, a name
    /// won't encode, or similar.
    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
}

/// Bytes [`Format::decode`] has matched to a kind's magic, handed on to
/// [`Format::decode_body`]. Only this crate makes one.
#[derive(Debug, Clone, Copy)]
pub struct Checked<'a>(&'a [u8]);

impl<'a> Checked<'a> {
    /// The whole file, magic included.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.0
    }
}
