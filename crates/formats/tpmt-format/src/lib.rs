//! The shape every file format shares: bytes in (decode), a struct out,
//! and back again (encode).
//!
//! Each format crate implements [`Format`] on its decoded struct, so
//! `Archive::decode(bytes)` and `archive.encode()` read the same everywhere.
//! Knows nothing about projects or pipelines.
//!
//! Every file's opening magic lives in [`FileKind`] and is checked only here.
//! Magics inside a file, like section tags, belong to that format's crate.
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
    /// Which kind this is, and so which magic the file opens with.
    const KIND: FileKind;

    type Error: std::error::Error + From<WrongKind>;

    /// Whether `data` opens with this format's magic.
    ///
    /// Says nothing about whether the rest is intact; that is
    /// [`decode`](Self::decode)'s job. Split out so a caller can pick a format
    /// before committing to it.
    #[must_use]
    fn recognises(data: &[u8]) -> bool {
        Self::KIND.matches(data)
    }

    /// Takes the file apart.
    ///
    /// # Errors
    ///
    /// [`WrongKind`] when `data` doesn't open with this format's magic, and
    /// whatever [`decode_body`](Self::decode_body) returns otherwise.
    fn decode(data: &'a [u8]) -> Result<Self, Self::Error> {
        if !Self::recognises(data) {
            return Err(WrongKind {
                expected: Self::KIND,
            }
            .into());
        }
        Self::decode_body(data)
    }

    /// Takes apart a file [`decode`](Self::decode) has already matched to
    /// this format, so an error out of it always means "this format, but
    /// broken", never "not this format". Call `decode` instead.
    ///
    /// # Errors
    ///
    /// When the file is broken. Each format's own error type says how.
    fn decode_body(data: &'a [u8]) -> Result<Self, Self::Error>;

    /// Writes the file back out.
    ///
    /// # Errors
    ///
    /// When the value doesn't fit the format: a size field overflows, a name
    /// won't encode, or similar.
    fn encode(&self) -> Result<Vec<u8>, Self::Error>;
}

/// Data handed to a decoder that doesn't open with its magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrongKind {
    pub expected: FileKind,
}

impl std::fmt::Display for WrongKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a {} file", self.expected.name())
    }
}

impl std::error::Error for WrongKind {}

/// A format the toolkit reads, told apart by the magic it opens with.
///
/// Every opening magic is defined here and nowhere else, so something that
/// only needs to tell formats apart, like an unpack sorting files, depends on this crate
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    /// An archive, owned by `JKernel` and decoded by `tpmt-jkernel-arc`.
    Rarc,
    /// A compression wrapper, owned by `JKernel` and decoded by
    /// `tpmt-jkernel-compress`.
    Yaz0,
    /// A message file, owned by `JMessage` and decoded by `tpmt-jmessage`.
    Mesg,
}

impl FileKind {
    pub const ALL: [Self; 3] = [Self::Rarc, Self::Yaz0, Self::Mesg];

    /// The kind whose magic `data` opens with, if any.
    #[must_use]
    pub fn identify(data: &[u8]) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.matches(data))
    }

    /// Whether `data` opens with this kind's magic.
    #[must_use]
    pub fn matches(self, data: &[u8]) -> bool {
        data.starts_with(&self.magic())
    }

    #[must_use]
    pub const fn magic(self) -> [u8; 4] {
        match self {
            Self::Rarc => *b"RARC",
            Self::Yaz0 => *b"Yaz0",
            Self::Mesg => *b"MESG",
        }
    }

    /// A stable lowercase name, for a file that records kinds.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rarc => "rarc",
            Self::Yaz0 => "yaz0",
            Self::Mesg => "mesg",
        }
    }

    /// The kind [`name`](Self::name) gave, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}
