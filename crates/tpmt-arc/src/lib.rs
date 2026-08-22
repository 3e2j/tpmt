//! RARC archive containers (likely "Resource Archives").
//!
//! Distributed as `.arc`, these are containers of game assets: a directory
//! tree and the bytes of every file in it.
//! Nearly every archive on the disc arrives Yaz0-compressed, but that wrapper
//! comes off before anything here sees it. Container only: what any of it holds
//! is somebody else's problem.
//!
//! # Why
//!
//! **Archives exist to package together closely related data, simplifying the
//! mounting/unmounting process**
//!
//! - An archive is loaded as itself, not the files in it.
//!   The game holds a refcount, and unloads the whole container when unused.
//! - Every file carries a flag for which memory pool it loads into:
//!   main (fast), auxiliary (slower), or read from disc (super slow).
//!
//! # External quirks to know
//!
//! - The memory flag is a request, not a guarantee. The code doing the
//!   mounting decides how much notice to take of it (most of the time this
//!   flag is ignored).
//! - An archive resolves cross-references between files within it. Every file carries
//!   an ID, and other files reference it by that number rather than by path.
//! - IDs can be ordered (marked by a `synced` bool) allowing for either O(1) lookups
//!   or extensive searches if unordered. Freshly authored archives are always ordered;
//!   an archive only goes unsynced through post-build editing measures, official or otherwise.
//!
//! # Example
//!
//! Replacing one file's bytes, which is the shape nearly every caller wants:
//!
//! ```
//! use tpmt_arc::{Archive, File};
//!
//! # let on_disc = tpmt_arc::pack(&Archive {
//! #     root: "archive".into(),
//! #     files: vec![File {
//! #         path: "dat/hello.bin".into(),
//! #         data: b"before",
//! #         ..Default::default()
//! #     }],
//! #     ..Default::default()
//! # })?;
//! let mut opened = tpmt_arc::unpack(&on_disc)?;
//! opened.files[0].data = b"after";
//! let rebuilt = tpmt_arc::pack(&opened)?;
//!
//! assert_eq!(tpmt_arc::unpack(&rebuilt)?.files[0].data, b"after");
//! # Ok::<(), tpmt_arc::Error>(())
//! ```

use serde::{Deserialize, Serialize};

pub mod editable;

mod pack;
mod unpack;

pub use pack::pack;
pub use unpack::unpack;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a RARC archive")]
    NotRarc,

    #[error("the archive is corrupt: {0}")]
    Corrupt(&'static str),

    // Checked both ways: a stored name on its way to becoming a path
    // component, and a caller's path component on its way to being stored.
    #[error("`{0}` is not usable as a file name")]
    UnusableName(String),

    #[error("the packed archive would not fit the format's size fields")]
    Oversized,

    // Only a hand-built file list can trip this; anything out of `unpack` is
    // grouped already. See `pack` for why the order is forced.
    #[error("the files are not in memory order: main memory, then ARAM, then disc")]
    Ungrouped,

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),

    #[error("the sidecar is not readable: {0}")]
    Sidecar(#[from] toml::de::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which console memory a file is loaded into when its archive is mounted.
///
/// The order the variants are declared in is the order an archive stores them
/// in, and [`pack`] holds callers to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preload {
    /// Main memory.
    #[default]
    Mram,
    /// Auxiliary memory: the console's second pool, reached over DMA rather
    /// than mapped, so slower to read from. Used to park mostly dormant code,
    /// increasing main ram headroom. Still faster than reading from disc.
    Aram,
    /// Not preloaded at all, read off the disc on demand.
    Disc,
}

/// One file in an archive, with its path relative to the archive root.
///
/// `id` and `preload` are the only things an entry records about a file that
/// its path and bytes do not say. They ride along so that a file handed from
/// [`unpack`] to [`pack`] comes back exactly as stored; a newly minted file
/// takes both from `..Default::default()`.
#[derive(Debug, Clone, Default)]
pub struct File<'a> {
    pub path: String,
    pub data: &'a [u8],
    /// The stored file id, used by *other* resources to cross-reference files.
    ///
    /// Treated as authored data, never reassigned. `None` is for a file nothing
    /// refers to yet, and takes its entry index from [`pack`], which is what a
    /// freshly authored archive numbers everything.
    // TODO: derive ids from the referencing resources once there is a linker.
    pub id: Option<u16>,
    pub preload: Preload,
}

/// An archive taken apart: the root directory's name, and every file under it.
#[derive(Debug, Clone, Default)]
pub struct Archive<'a> {
    /// The root directory's name, stored independently of the archive's own
    /// file name and free to differ from it, so a rebuild has to be handed it
    /// back. Has no effect on the file tree or its naming.
    ///
    /// This is the name the archive mounts under, which is why it is worth
    /// preserving beyond a byte-exact rebuild.
    pub root: String,
    pub files: Vec<File<'a>>,
    /// Leftover bookkeeping from whatever built the archive, which nothing
    /// reads. [`pack`] derives it, so this is `Some` only for the few archives
    /// storing a number that would not come back on its own.
    pub next_free_id: Option<u16>,
}

/// The fixed 0x20 at the front of the archive. Everything else is found
/// through it.
mod top_header {
    pub const LEN: usize = 0x20;
    /// What 0x00 holds, which is the only way to tell an archive from anything
    /// else handed to [`unpack`](super::unpack).
    pub const MAGIC: &[u8; 4] = b"RARC";
    pub const FILE_SIZE: usize = 0x04;
    pub const DATA_HEADER_PTR: usize = 0x08;
    /// A field of this header, but counted from the data header like
    /// everything below. References disagree on whether the anchor is the data
    /// header or 0x20; every retail archive pins the data header at 0x20,
    /// where nothing distinguishes the two.
    pub const FILE_DATA_PTR: usize = 0x0C;
    pub const TOTAL_DATA_SIZE: usize = 0x10;
    pub const MRAM_SIZE: usize = 0x14;
    pub const ARAM_SIZE: usize = 0x18;
    // Nothing names 0x1C, which is zero on every retail archive.
}

/// What the top header points at. Every offset in it, and the file data offset
/// above, is counted from where this header starts.
mod data_header {
    /// It follows the top header, so it starts one header in.
    pub const AT: usize = super::top_header::LEN;
    pub const LEN: usize = 0x20;
    pub const NODE_COUNT: usize = 0x00;
    pub const NODE_LIST_PTR: usize = 0x04;
    pub const ENTRY_COUNT: usize = 0x08;
    pub const ENTRY_LIST_PTR: usize = 0x0C;
    pub const STRING_POOL_SIZE: usize = 0x10;
    pub const STRING_POOL_PTR: usize = 0x14;
    pub const NEXT_FREE_ID: usize = 0x18;
    pub const SYNCED_IDS: usize = 0x1A;
    // The five bytes after the sync flag are unnamed and zero.
}

/// One directory's record, in the list the data header points at.
mod node {
    pub const LEN: usize = 0x10;
    // 0x00 is a four character tag, so the first named field is 0x04.
    pub const NAME: usize = 0x04;
    pub const NAME_HASH: usize = 0x08;
    /// `.`, `..` and subdirectories are counted here too, not just files.
    pub const ENTRY_COUNT: usize = 0x0A;
    pub const FIRST_ENTRY: usize = 0x0C;
}

/// One file's or one directory's record. A directory's points at its node, a
/// file's at its bytes.
mod entry {
    pub const LEN: usize = 0x14;
    pub const ID: usize = 0x00;
    pub const NAME_HASH: usize = 0x02;
    /// Carries two things: the flags in the top byte, the offset of the
    /// entry's name in the string pool in the remaining three.
    pub const FLAGS_AND_NAME: usize = 0x04;
    pub const DATA_OR_NODE: usize = 0x08;
    pub const DATA_SIZE: usize = 0x0C;
    // The last 0x04 of a record are always zero.

    // What splits the field above into its two halves.
    pub const FLAGS_SHIFT: u32 = 24;
    pub const NAME_MASK: u32 = 0x00FF_FFFF;

    // The flags themselves, once shifted down. An entry is a file or a
    // directory, a file is preloaded into one of the three memories, and the
    // last two say the bytes are compressed and which of the two schemes did it.
    pub const FLAG_FILE: u32 = 0x01;
    pub const FLAG_DIRECTORY: u32 = 0x02;
    pub const FLAG_COMPRESSED: u32 = 0x04;
    pub const FLAG_MRAM: u32 = 0x10;
    pub const FLAG_ARAM: u32 = 0x20;
    pub const FLAG_DISC: u32 = 0x40;
    pub const FLAG_YAZ0: u32 = 0x80;

    /// A directory entry has no bytes, but its size field still says 0x10 on
    /// every retail archive, presumably the record's own size.
    pub const DIRECTORY_SIZE: u32 = 0x10;
    /// Directories share one id, which is no id at all.
    pub const NO_ID: u16 = 0xFFFF;
}

/// One past the highest id in use, counting entries rather than files when the
/// ids are all their own entry index, since then the directories sit on ids too.
///
/// Worked out the same way at both ends, so that [`unpack`] can tell an archive
/// storing this from one storing something else.
fn next_free_id(entry_count: usize, highest: Option<u16>, synced: bool) -> Result<u16> {
    match synced {
        true => u16::try_from(entry_count),
        false => u16::try_from(highest.map_or(0, |id| u32::from(id) + 1)),
    }
    .map_err(|_| Error::Oversized)
}

/// The hash stored beside every name reference: each byte folded onto three
/// times the running total.
fn name_hash(name: &[u8]) -> u16 {
    name.iter().fold(0, |hash, &byte| {
        hash.wrapping_mul(3).wrapping_add(byte as u16)
    })
}
