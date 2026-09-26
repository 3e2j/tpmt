//! Which version of the game is running, and in which language.
//!
//! The decomp branches on `REGION_*`, `PLATFORM_*` and `VERSION`.
//! As region and platform are subsets of versions, we just branch
//! on [`Versions`] only.
//!
//! The disc fixes the version. The language is the one a message file was
//! written in, which its `/res/Msg**` folder names, so an [`Edition`] carries
//! both.

/// One version of the game.
/// Based on the disc ID and Revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Version {
    // Each discriminant is the version's bit in `Versions`.
    WiiUsaRev0,
    WiiPal,
    WiiJpn,
    GcnUsa,
    GcnPal,
    GcnJpn,
    /// `RZDE` revision 2.
    WiiUsa,
    /// The kiosk demo, `DZDE`. The decomp's `VERSION_WII_USA_KIOSK`.
    WiiUsaKiosk,
    // No `VERSION_WII_KOR`, since there's no disc for it.
    // Nvidia Shield editions (Shield, ShieldD) excluded.
}

impl Version {
    /// Every version, for a picker to list.
    pub const ALL: [Self; 8] = [
        Self::WiiUsaRev0,
        Self::WiiPal,
        Self::WiiJpn,
        Self::GcnUsa,
        Self::GcnPal,
        Self::GcnJpn,
        Self::WiiUsa,
        Self::WiiUsaKiosk,
    ];

    /// The version a disc's game id and revision name, or `None` when they
    /// name none, like `RZDE` revision 1.
    ///
    /// The maker code isn't read, since it's `01` on every disc.
    #[must_use]
    pub fn from_disc(id: &str, revision: u8) -> Option<Self> {
        match (id, revision) {
            ("GZ2E", _) => Some(Self::GcnUsa),
            ("GZ2P", _) => Some(Self::GcnPal),
            ("GZ2J", _) => Some(Self::GcnJpn),
            ("RZDE", 0) => Some(Self::WiiUsaRev0),
            ("RZDE", 2) => Some(Self::WiiUsa),
            ("RZDP", _) => Some(Self::WiiPal),
            ("RZDJ", _) => Some(Self::WiiJpn),
            ("DZDE", _) => Some(Self::WiiUsaKiosk),
            _ => None,
        }
    }

    /// The languages the version ships text for, first being the default.
    /// Sourced from Dusklight's `available_languages`.
    #[must_use]
    pub const fn languages(self) -> &'static [Language] {
        use Language::{English, French, German, Italian, Japanese, Spanish};
        if Versions::JPN.contains(self) {
            &[Japanese]
        } else if Versions::PAL.contains(self) {
            &[English, German, French, Spanish, Italian]
        } else if matches!(self, Self::WiiUsa) {
            &[English, French, Spanish]
        } else {
            // Dusklight doesn't list the kiosk. English only is a guess from
            // the other USA versions.
            &[English]
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of versions, the one thing every version split in the game picks
/// by. Wide enough for versions not listed yet, like Wii KOR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Versions(u16);

impl Versions {
    pub const ALL: Self = Self::of(&Version::ALL);
    pub const GCN: Self = Self::of(&[Version::GcnUsa, Version::GcnPal, Version::GcnJpn]);
    pub const WII: Self = Self::GCN.complement();
    pub const JPN: Self = Self::of(&[Version::GcnJpn, Version::WiiJpn]);
    pub const PAL: Self = Self::of(&[Version::GcnPal, Version::WiiPal]);
    pub const USA: Self = Self::JPN.union(Self::PAL).complement();
    /// `VERSION == VERSION_GCN_PAL`, which a few message tags test alone.
    pub const GCN_PAL: Self = Self::of(&[Version::GcnPal]);

    #[must_use]
    pub const fn of(versions: &[Version]) -> Self {
        let mut bits = 0;
        let mut at = 0;
        while at < versions.len() {
            bits |= versions[at].bit();
            at += 1;
        }
        Self(bits)
    }

    #[must_use]
    pub const fn contains(self, version: Version) -> bool {
        self.0 & version.bit() != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Every listed version not in `self`.
    #[must_use]
    pub const fn complement(self) -> Self {
        Self(!self.0 & Self::ALL.0)
    }

    #[must_use]
    pub const fn is_disjoint(self, other: Self) -> bool {
        self.0 & other.0 == 0
    }
}

/// A language a version can run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    German,
    French,
    Spanish,
    Italian,
    Japanese,
}

/// A version, and one of the languages it ships text in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edition {
    version: Version,
    language: Language,
}

impl Edition {
    /// `None` when `version` ships no text in `language`.
    #[must_use]
    pub const fn new(version: Version, language: Language) -> Option<Self> {
        let languages = version.languages();
        let mut at = 0;
        while at < languages.len() {
            if languages[at] as u8 == language as u8 {
                return Some(Self { version, language });
            }
            at += 1;
        }
        None
    }

    /// `version` in its default language.
    #[must_use]
    pub const fn default_language(version: Version) -> Self {
        Self {
            version,
            language: version.languages()[0],
        }
    }

    #[must_use]
    pub const fn version(self) -> Version {
        self.version
    }

    #[must_use]
    pub const fn language(self) -> Language {
        self.language
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the Wii USA disc tells its versions apart by revision.
    #[test]
    fn only_rzde_reads_the_revision() {
        assert_eq!(Version::from_disc("GZ2E", 1), Some(Version::GcnUsa));
        assert_eq!(Version::from_disc("RZDE", 2), Some(Version::WiiUsa));
        assert_eq!(Version::from_disc("RZDE", 1), None);
    }

    #[test]
    fn the_regions_split_every_version_once() {
        for version in Version::ALL {
            let regions = [Versions::USA, Versions::PAL, Versions::JPN];
            let holding = regions.iter().filter(|region| region.contains(version));
            assert_eq!(holding.count(), 1, "{version:?}");
        }
    }

    #[test]
    fn a_platform_is_the_other_ones_complement() {
        assert!(Versions::GCN.is_disjoint(Versions::WII));
        assert_eq!(Versions::GCN.union(Versions::WII), Versions::ALL);
        assert!(Versions::WII.contains(Version::WiiUsaKiosk));
    }

    #[test]
    fn an_edition_needs_a_language_its_version_ships() {
        assert!(Edition::new(Version::GcnPal, Language::German).is_some());
        assert!(Edition::new(Version::WiiUsa, Language::Spanish).is_some());
        assert!(Edition::new(Version::GcnUsa, Language::Spanish).is_none());
        assert!(Edition::new(Version::GcnJpn, Language::English).is_none());
    }
}
