//! The archive sidecar, at the root of every directory an archive was unpacked
//! into.
//!
//! - Pre-assigned member IDs used for cross-referencing.
//! - Which memory each member is loaded into.
//! - The name the root node goes back under.
//! - Yaz0 wrapper presence (for archive and members).
//!
//! The order of the members is data too. An archive lays its file bytes out in
//! the order its entries come, and the two preload runs the header describes
//! are stretches of that order, so a rebuild that reordered them would describe
//! the wrong bytes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::fs::{read, write};

/// Sits at the root of every unpacked archive.
pub(crate) const SIDECAR: &str = ".tpmt-arc.toml";

/// What an archive nobody named calls its root.
///
/// The name is not how anything finds a file inside, so an archive that never
/// existed can carry the plainest one there is.
const ROOT: &str = "archive";

/// What an archive is, minus the bytes.
///
/// Only two things here are the archive's own. Everything the format records
/// per file sits on [`Member`] instead, because that is where the archive keeps
/// it: an entry each, not a setting for the container.
///
/// The next-free-id counter is the one thing an archive carries that is left
/// out. It is bookkeeping belonging to whatever built the archive, nothing
/// reads it, and a rebuild derives one that is just as good, so the handful of
/// archives storing an unusual number come back two bytes off rather than
/// earning a key nobody would ever set.
#[derive(Serialize, Deserialize)]
pub(crate) struct Manifest {
    /// The name the archive's root directory carries inside the archive, which
    /// is rarely the file name. See [`tpmt_arc::Archive::root`] for why it is
    /// worth keeping.
    pub(crate) root: String,
    /// Whether a Yaz0 wrapper came off this archive on the way in.
    #[serde(default)]
    pub(crate) yaz0_compressed: bool,
    /// Every member, in the order the archive stored them.
    #[serde(default, rename = "member")]
    pub(crate) members: Vec<Member>,
}

/// One member, at the path it sits at under the archive root.
///
/// Every field below one is the member's own. The archive gives each file an
/// entry of its own and writes all of this into it, so two files side by side
/// can differ in all of it. The header's two preload sizes look like archive
/// settings and are not: they are totals, added up from these.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Member {
    pub(crate) path: String,
    /// Which memory the game loads this one file into.
    pub(crate) preload: Preload,
    /// Whether this member's bytes were Yaz0 wrapped inside the archive.
    ///
    /// Recorded because unpack takes the wrapper off members before writing
    /// the member out.
    #[serde(default, skip_serializing_if = "not_compressed")]
    pub(crate) yaz0_compressed: bool,
    /// The id the game asks for this member by.
    ///
    /// Left out only for a member nothing has named, which is one somebody put
    /// in the directory themselves. ID for these are auto-assigned after
    /// pre-existing IDs have been fixed in place
    // TODO: Adding a linker functionality to stop awkward "authored" storing.
    // ID's don't have to be stored in an authored state if the files that
    // reference the resources are handled too. We already assemble archives
    // last, so gathering which files reference which members allows us to
    // dynamically hand out IDs to satisfy those references instead of
    // preserving the ones that shipped.
    //
    // What makes it awkward is that a reference is a bare number: an `.stb`
    // names no member, only the id it was authored against, so unpack would
    // have to record which member each id landed on and write the reference
    // down as that member. Ids would stop being authored data and become an
    // allocation, and this field would go back to being an override. Worth a
    // switch in the project config whichever way it lands, since rewriting
    // somebody else's file to suit an archive is a bigger claim to make than
    // repacking one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<u16>,
}

/// Mirrors [`tpmt_arc::Preload`]. Kept here rather than derived over there so
/// that the archive format does not take on a serialisation format to suit how
/// this pipeline happens to store projects.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Preload {
    /// Main memory.
    #[default]
    Mram,
    /// Auxiliary memory.
    Aram,
    /// Not preloaded, read on demand.
    Disc,
}

impl From<tpmt_arc::Preload> for Preload {
    fn from(preload: tpmt_arc::Preload) -> Self {
        match preload {
            tpmt_arc::Preload::Mram => Self::Mram,
            tpmt_arc::Preload::Aram => Self::Aram,
            tpmt_arc::Preload::Disc => Self::Disc,
        }
    }
}

impl From<Preload> for tpmt_arc::Preload {
    fn from(preload: Preload) -> Self {
        match preload {
            Preload::Mram => Self::Mram,
            Preload::Aram => Self::Aram,
            Preload::Disc => Self::Disc,
        }
    }
}

fn not_compressed(yaz0_compressed: &bool) -> bool {
    !yaz0_compressed
}

impl Member {
    /// A member the sidecar never named, which is one somebody put in the
    /// directory themselves. Main memory is where a file with nothing saying
    /// otherwise belongs, and an id it was never stored under is one the
    /// rebuild works out.
    pub(crate) fn new(path: String) -> Self {
        Self {
            path,
            preload: Preload::Mram,
            yaz0_compressed: false,
            id: None,
        }
    }
}

impl Manifest {
    pub(crate) fn new(root: String, yaz0_compressed: bool, members: Vec<Member>) -> Self {
        Self {
            root,
            yaz0_compressed,
            members,
        }
    }

    /// An archive nobody has described, which is every directory somebody makes
    /// and calls `.arc`.
    ///
    /// Nothing here is guessed: one that never existed has no root name to
    /// recover, no wrapping it arrived under and no members it used to hold.
    /// The files in the directory become members on their own terms.
    pub(crate) fn fresh() -> Self {
        Self::new(ROOT.to_string(), false, Vec::new())
    }

    /// Reads the sidecar at the root of an unpacked archive.
    ///
    /// A missing one is an error here rather than a [`fresh`](Self::fresh)
    /// archive, because on a directory that came off the disc it means the
    /// sidecar was lost: defaulting there would quietly throw away the memory
    /// every member was loaded into and the wrapper the archive arrived under.
    /// Only a caller that knows the disc never had this archive can say the
    /// missing file is nothing to worry about.
    pub(crate) fn read(directory: &Path) -> Result<Self, Error> {
        let path = directory.join(SIDECAR);
        if !path.exists() {
            return Err(Error::LostSidecar(path));
        }

        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        let manifest: Self = toml::from_str(&text).map_err(|source| Error::UnreadableProject {
            path: path.clone(),
            source,
        })?;

        Ok(manifest)
    }

    /// Writes the sidecar, giving back the bytes it wrote, since change
    /// detection hashes what went into the project rather than reading it back.
    pub(crate) fn write(&self, directory: &Path) -> Result<Vec<u8>, Error> {
        let text = toml::to_string_pretty(self)?;
        write(&directory.join(SIDECAR), text.as_bytes())?;
        Ok(text.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Manifest {
        Manifest::new(
            "archive".to_string(),
            true,
            vec![
                Member {
                    path: "model.bdl".to_string(),
                    preload: Preload::Mram,
                    yaz0_compressed: false,
                    id: Some(0),
                },
                Member {
                    path: "sound/bgm.aw".to_string(),
                    preload: Preload::Aram,
                    yaz0_compressed: true,
                    id: Some(3),
                },
            ],
        )
    }

    /// What a person opens when they want a file moved into auxiliary memory.
    /// The quiet keys stay quiet: a member nothing was wrapped around says nothing
    /// about wrapping.
    #[test]
    fn round_trips_through_toml() {
        let text = toml::to_string_pretty(&example()).unwrap();
        assert_eq!(
            text,
            "root = \"archive\"\n\
             yaz0_compressed = true\n\
             \n\
             [[member]]\n\
             path = \"model.bdl\"\n\
             preload = \"mram\"\n\
             id = 0\n\
             \n\
             [[member]]\n\
             path = \"sound/bgm.aw\"\n\
             preload = \"aram\"\n\
             yaz0_compressed = true\n\
             id = 3\n"
        );

        let read: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(read.root, "archive");
        assert!(read.yaz0_compressed);
        assert_eq!(read.members[0].id, Some(0));
        assert!(!read.members[0].yaz0_compressed);
        assert_eq!(read.members[1].preload, Preload::Aram);
        assert!(read.members[1].yaz0_compressed);
        assert_eq!(read.members[1].id, Some(3));
    }

    /// An archive somebody wrote themselves is a root name and a list of names.
    /// Everything else has an answer already.
    #[test]
    fn a_hand_written_one_can_leave_the_rest_out() {
        let read: Manifest = toml::from_str(
            "root = \"archive\"\n\
             \n\
             [[member]]\n\
             path = \"new.bin\"\n\
             preload = \"mram\"\n",
        )
        .unwrap();

        assert!(!read.yaz0_compressed);
        assert!(!read.members[0].yaz0_compressed);
        assert_eq!(read.members[0].id, None);
    }
}
