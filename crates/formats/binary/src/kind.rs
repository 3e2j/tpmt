//! Telling files apart by the magic they open with, before anything decodes
//! them.
//!
//! Every file's opening magic lives in [`FileKind`] and is checked only here.
//! Magics inside a file, like section tags, belong to that format's crate.
//!
//! [`Compression`] groups the kinds that wrap another file.

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
    /// An archive, owned by `JKernel` and decoded by `tpmt-archive`.
    Rarc,
    /// A compression wrapper, owned by `JKernel` and decoded by
    /// `tpmt-compression`.
    Yaz0,
    /// The other compression wrapper, the same way.
    Yay0,
    /// A message file, owned by `JMessage` and decoded by `tpmt-message`.
    Mesg,
}

impl FileKind {
    pub const ALL: [Self; 4] = [Self::Rarc, Self::Yaz0, Self::Yay0, Self::Mesg];

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

    /// Checks that `data` opens with this kind's magic, for a decoder to
    /// open with.
    ///
    /// # Errors
    ///
    /// [`WrongKind`] when it doesn't.
    pub fn check(self, data: &[u8]) -> Result<(), WrongKind> {
        self.matches(data)
            .then_some(())
            .ok_or(WrongKind { expected: self })
    }

    #[must_use]
    pub const fn magic(self) -> [u8; 4] {
        match self {
            Self::Rarc => *b"RARC",
            Self::Yaz0 => *b"Yaz0",
            Self::Yay0 => *b"Yay0",
            Self::Mesg => *b"MESG",
        }
    }

    /// A stable lowercase name, for a file that records kinds.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rarc => "rarc",
            Self::Yaz0 => "yaz0",
            Self::Yay0 => "yay0",
            Self::Mesg => "mesg",
        }
    }

    /// The kind [`name`](Self::name) gave, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The compression wrapper this kind is, if it is one.
    #[must_use]
    pub const fn compression(self) -> Option<Compression> {
        match self {
            Self::Yaz0 => Some(Compression::Yaz0),
            Self::Yay0 => Some(Compression::Yay0),
            Self::Rarc | Self::Mesg => None,
        }
    }

    /// Whether this kind packages other files, as a compression wrapper or an
    /// archive does, and isn't a payload itself.
    #[must_use]
    pub const fn is_packaging(self) -> bool {
        // disc crate excluded from here as it doesn't have a consistent magic
        // nor does it implement the Format trait
        self.compression().is_some() || matches!(self, Self::Rarc)
    }
}

/// A compression wrapper, the subset of formats a container can say its
/// contents are in.
///
/// With the `serde` feature, a record names one the way [`FileKind::name`]
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
pub enum Compression {
    Yaz0,
    /// No retail archive marks a file with it.
    Yay0,
}

impl Compression {
    /// The wrapper `data` opens with, told by its magic.
    #[must_use]
    pub fn of(data: &[u8]) -> Option<Self> {
        FileKind::identify(data).and_then(FileKind::compression)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Yaz0 => "Yaz0",
            Self::Yay0 => "Yay0",
        }
    }
}
