//! One module per build target, each owning a directory of the same name
//! under `build/targets/`.
//!
//! Every target re-encodes the overlay's changes with [`super::rebuild`] and
//! packages the result its own way.

use std::fmt;

pub mod dusk;
pub mod image;
pub mod patch;

/// What a build is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The changed disc files, at the paths a disc holds them under.
    Patch,
    /// A whole playable disc image.
    Image,
    /// A Dusklight mod bundle.
    Dusk,
}

impl Target {
    pub const ALL: [Self; 3] = [Self::Patch, Self::Image, Self::Dusk];

    /// What the target is called, which is also the directory it owns.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Patch => "patch",
            Self::Image => "image",
            Self::Dusk => "dusk",
        }
    }

    /// The target called `name`, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.name() == name)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
