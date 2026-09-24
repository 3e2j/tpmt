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

/// A file format: bytes in, `Self` out, and back again.
///
/// `'a` is the input's lifetime, for a decoded form that borrows from it
/// (an archive keeps a slice into every member). One that copies everything
/// out implements `Format<'_>`.
pub trait Format<'a>: Sized {
    /// What the file opens with, and the only way to tell one format from
    /// another: paths and extensions on the disc lie, contents don't.
    const MAGIC: &'static [u8];

    type Error: std::error::Error;

    /// Whether `data` opens with this format's [`MAGIC`](Self::MAGIC).
    ///
    /// Says nothing about whether the rest is intact; that is
    /// [`decode`](Self::decode)'s job. Split out so a caller can pick a format
    /// before committing to it, and so an error out of `decode` always means
    /// "this format, but broken", never "not this format".
    #[must_use]
    fn recognises(data: &[u8]) -> bool {
        data.starts_with(Self::MAGIC)
    }

    /// Takes the file apart.
    ///
    /// # Errors
    ///
    /// When `data` is not this format at all, or is but is broken. Each
    /// format's own error type tells the two apart.
    fn decode(data: &'a [u8]) -> Result<Self, Self::Error>;

    /// Writes the file back out.
    ///
    /// # Errors
    ///
    /// When the value doesn't fit the format: a size field overflows, a name
    /// won't encode, or similar.
    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
}
