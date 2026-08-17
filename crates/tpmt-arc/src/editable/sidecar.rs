//! What an archive is, minus its bytes.
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
//!
//! TOML, so a project can keep this as text alongside an unpacked archive.
//! Where it sits and what a missing one means belongs to whoever stores a
//! project; that it round-trips through TOML at all is this crate's own
//! call.

use serde::{Deserialize, Serialize};

use crate::Preload;

/// The name a project keeps this under, at the root of every directory an
/// archive was unpacked into.
pub const SIDECAR: &str = ".tpmt-arc.toml";

/// What an archive nobody named calls its root.
///
/// The name is not how anything finds a file inside, so an archive that never
/// existed can carry the plainest one there is.
const ROOT: &str = "archive";

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
pub struct Sidecar {
    /// The name the archive's root directory carries inside the archive, which
    /// is rarely the file name. See [`crate::Archive::root`] for why it is
    /// worth keeping.
    pub root: String,
    /// Whether a Yaz0 wrapper came off this archive on the way in.
    #[serde(default)]
    pub yaz0_compressed: bool,
    /// Every member, in the order the archive stored them.
    #[serde(default, rename = "member")]
    pub members: Vec<Member>,
}

/// One member, at the path it sits at under the archive root.
///
/// Every field below one is the member's own. The archive gives each file an
/// entry of its own and writes all of this into it, so two files side by side
/// can differ in all of it. The header's two preload sizes look like archive
/// settings and are not: they are totals, added up from these.
#[derive(Serialize, Deserialize, Clone)]
pub struct Member {
    pub path: String,
    /// Which memory the game loads this one file into.
    pub preload: Preload,
    /// Whether this member's bytes were Yaz0 wrapped inside the archive.
    ///
    /// Recorded because unpack takes the wrapper off members before writing
    /// the member out.
    #[serde(default, skip_serializing_if = "not_compressed")]
    pub yaz0_compressed: bool,
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
    //
    // Resolving references belongs in tpmt-pipeline, not here, same as the
    // sidecar itself: it takes seeing every format to know who references
    // what. This crate would only claim an id it's handed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u16>,
}

fn not_compressed(yaz0_compressed: &bool) -> bool {
    !yaz0_compressed
}

impl Member {
    /// A member the sidecar never named, which is one somebody put in the
    /// directory themselves. Main memory is where a file with nothing saying
    /// otherwise belongs, and an id it was never stored under is one the
    /// rebuild works out.
    pub fn new(path: String) -> Self {
        Self {
            path,
            preload: Preload::Mram,
            yaz0_compressed: false,
            id: None,
        }
    }
}

impl Sidecar {
    pub fn new(root: String, yaz0_compressed: bool, members: Vec<Member>) -> Self {
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
    pub fn fresh() -> Self {
        Self::new(ROOT.to_string(), false, Vec::new())
    }

    /// The TOML text a project keeps this as.
    ///
    /// Every field on a sidecar is a plain serializable shape (strings,
    /// bools, an enum, options, a vec of the same), which `toml` only ever
    /// fails to serialize over a NaN float or a non-string map key, neither
    /// of which this has, so there is no `Result` to hand back.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("a Sidecar always serializes")
    }

    /// Reads a sidecar back out of the TOML text a project kept it as.
    pub fn from_toml(text: &str) -> crate::Result<Self> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Sidecar {
        Sidecar::new(
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
        let text = example().to_toml();
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

        let read = Sidecar::from_toml(&text).unwrap();
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
        let read = Sidecar::from_toml(
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
