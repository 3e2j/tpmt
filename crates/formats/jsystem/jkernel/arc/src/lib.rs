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
//! use tpmt_jkernel_arc::{Archive, File, Format};
//!
//! # let on_disc = Archive {
//! #     root: "archive".into(),
//! #     files: vec![File {
//! #         path: "dat/hello.bin".into(),
//! #         data: b"before",
//! #         ..Default::default()
//! #     }],
//! #     ..Default::default()
//! # }
//! # .encode()?;
//! let mut opened = Archive::decode(&on_disc)?;
//! opened.files[0].data = b"after";
//! let rebuilt = opened.encode()?;
//!
//! assert_eq!(Archive::decode(&rebuilt)?.files[0].data, b"after");
//! # Ok::<(), tpmt_jkernel_arc::Error>(())
//! ```

use serde::{Deserialize, Serialize};
use tpmt_bytes::{Be16, Be32, Flag, Layout};

pub mod editable;

mod pack;
mod unpack;

pub use tpmt_format::{FileKind, Format};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    WrongKind(#[from] tpmt_format::WrongKind),

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

    #[error("the sidecar could not be written: {0}")]
    UnwritableSidecar(#[from] toml::ser::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which console memory a file is loaded into when its archive is mounted.
///
/// The order the variants are declared in is the order an archive stores them
/// in, and [`Archive::encode`] holds callers to it.
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
/// [`Archive::decode`] to [`Archive::encode`] comes back exactly as stored; a newly minted file
/// takes both from `..Default::default()`.
#[derive(Debug, Clone, Default)]
pub struct File<'a> {
    pub path: String,
    pub data: &'a [u8],
    /// The stored file id, used by *other* resources to cross-reference files.
    ///
    /// Treated as authored data, never reassigned. `None` is for a file nothing
    /// refers to yet, and takes its entry index from [`Archive::encode`], which is what a
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
    /// reads. [`Archive::encode`] derives it, so this is `Some` only for the
    /// few archives storing a number that would not come back on its own.
    pub next_free_id: Option<u16>,
}

impl<'a> Format<'a> for Archive<'a> {
    const KIND: FileKind = FileKind::Rarc;
    type Error = Error;

    /// Takes an archive apart into every file it holds, directories flattened
    /// into the paths. Nothing is copied out of `data`.
    ///
    /// The files come back in the archive's own order, which is the order
    /// [`encode`](Self::encode) rebuilds the tree from, so a round trip keeps it.
    ///
    /// Compression flags are dropped, since `encode` recomputes them from the
    /// file's bytes.
    ///
    /// # Errors
    ///
    /// - [`Error::WrongKind`]
    /// - [`Error::UnusableName`]
    /// - [`Error::Corrupt`] if the archive's structure is corrupt in a way that
    ///   would misplace or lose an entry (a wrong stated size, a missing root,
    ///   more entries than it claims to hold, a file with no memory tag, a
    ///   directory tree that loops, or similar).
    fn decode_body(data: tpmt_format::Checked<'a>) -> Result<Self> {
        unpack::unpack(data.bytes())
    }

    /// Writes a whole archive from its file list.
    ///
    /// Directories only exist as shared prefixes of file paths, so an empty
    /// one is dropped: there's no path left to name it. The root is named by
    /// the archive itself, so an empty file list still packs.
    ///
    /// Files must arrive grouped by memory: every [`Preload::Mram`] one, then
    /// every [`Preload::Aram`] one, then the rest. The header stores one total
    /// size per memory rather than tagging each file, so that total is only
    /// correct if its group is contiguous; an interleaved list gets
    /// [`Error::Ungrouped`] instead of an archive with wrong stated sizes.
    /// Path order is unconstrained, and a list straight from
    /// [`decode`](Self::decode) is already grouped.
    ///
    /// Every field is reproduced. A file with no [`File::id`] gets the lowest
    /// id no other file in the list already claims, not the format's own
    /// convention (see the comment above `place_files` in `pack.rs` for why).
    /// This only guards against a collision within the list handed in; it
    /// cannot know whether some other file, elsewhere, still references an id
    /// that a deleted file used to hold. That is a linker's job once one
    /// exists.
    ///
    /// An [`Archive::next_free_id`] the input didn't carry is derived here too
    /// (despite never being used by our implementation).
    ///
    /// # Errors
    ///
    /// - [`Error::UnusableName`] if a path has an empty, `.`, `..`, or
    ///   backslash-holding component, or one that doesn't encode as Shift-JIS.
    /// - [`Error::Ungrouped`]
    /// - [`Error::Oversized`]
    fn encode(&self) -> Result<Vec<u8>> {
        pack::pack(self)
    }
}

tpmt_bytes::layout! {
    /// The fixed 0x20 at the front of the archive. Everything else is found
    /// through it.
    struct TopHeader {
        magic: [u8; 4],
        file_size: Be32,
        data_header_ptr: Be32,
        /// Counted from the data header, like the offsets below. Some
        /// references count it from 0x20 instead. Retail archives always put
        /// the data header at 0x20, so both readings agree.
        file_data_ptr: Be32,
        total_data_size: Be32,
        mram_size: Be32,
        aram_size: Be32,
        /// Unnamed, and zero on every retail archive.
        unnamed: [u8; 4],
    }
}

tpmt_bytes::layout! {
    /// What the top header points at. Every offset in it, and the file data
    /// offset above, is counted from where this header starts.
    struct DataHeader {
        node_count: Be32,
        node_list_ptr: Be32,
        entry_count: Be32,
        entry_list_ptr: Be32,
        string_pool_size: Be32,
        string_pool_ptr: Be32,
        next_free_id: Be16,
        synced_ids: Flag,
        /// Unnamed, and zero.
        unnamed: [u8; 5],
    }
}

impl DataHeader {
    /// It follows the top header, so it starts one header in.
    const AT: usize = TopHeader::LEN;
}

tpmt_bytes::layout! {
    /// One directory's record, in the list the data header points at.
    struct Node {
        /// A four character tag.
        tag: [u8; 4],
        name: Be32,
        name_hash: Be16,
        /// Counts `.`, `..` and subdirectories as well as files.
        entry_count: Be16,
        first_entry: Be32,
    }
}

tpmt_bytes::layout! {
    /// One file's or one directory's record. A directory's points at its node,
    /// a file's at its bytes.
    struct Entry {
        id: Be16,
        name_hash: Be16,
        flags: u8,
        /// The name's string pool offset, a 24-bit big-endian number.
        name: [u8; 3],
        data_or_node: Be32,
        data_size: Be32,
        /// Always zero.
        unnamed: [u8; 4],
    }
}

impl Entry {
    // Each flag is one bit of the entry's flags byte, read with `flags & FLAG`.
    // FLAG_YAZ0 only counts when FLAG_COMPRESSED is set.
    const FLAG_FILE: u8 = 0x01;
    const FLAG_DIRECTORY: u8 = 0x02;
    const FLAG_COMPRESSED: u8 = 0x04; // yaz0 or yay0
    // 0x08 unused
    const FLAG_MRAM: u8 = 0x10;
    const FLAG_ARAM: u8 = 0x20;
    const FLAG_DISC: u8 = 0x40;
    const FLAG_YAZ0: u8 = 0x80; // absence with FLAG_COMPRESSED is yay0 (unused)

    /// A directory entry has no bytes, but its size field still says 0x10 on
    /// every retail archive, presumably the record's own size.
    const DIRECTORY_SIZE: u32 = 0x10;
    /// Directories share one id, which is no id at all.
    const NO_ID: u16 = 0xFFFF;

    /// Where the name starts in the string pool.
    const fn name_offset(&self) -> u32 {
        let [high, mid, low] = self.name;
        u32::from_be_bytes([0, high, mid, low])
    }
}

/// A string pool offset as an entry's name field holds it, if it fits in 24
/// bits.
const fn name_field(offset: u32) -> Option<[u8; 3]> {
    match offset.to_be_bytes() {
        [0, high, mid, low] => Some([high, mid, low]),
        _ => None,
    }
}

/// One past the highest id in use, counting entries rather than files when the
/// ids are all their own entry index, since then the directories sit on ids too.
///
/// Worked out the same way at both ends, so that `decode` can tell an archive
/// storing this from one storing something else.
fn next_free_id(entry_count: usize, highest: Option<u16>, synced: bool) -> Result<u16> {
    if synced {
        u16::try_from(entry_count)
    } else {
        u16::try_from(highest.map_or(0, |id| u32::from(id) + 1))
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
