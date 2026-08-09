//! RARC archive containers (likely meaning "Resource" Archives).
//!
//! Distributed as `.arc`, these are containers of game assets: a directory
//! tree and the bytes of every file in it.
//! Nearly every archive on the disc arrives Yaz0-compressed, but that wrapper
//! comes off before anything here sees it. Container only: what any of it holds
//! is somebody else's problem.
//!
//! # Why
//!
//! **Archives exist to simplify the mounting/unmounting process for closely
//! related data.**
//!
//! - An archive is loaded/unloaded as itself, not the files in it.
//!   The game holds a refcount, and unloads the whole container when unused.
//! - Every file carries a flag for which memory pool it belongs in:
//!   main, auxiliary, or read from disc.
//!
//! # External quirks to know
//!
//! - The memory flag is a request, not a guarantee. The code doing the
//!   mounting decides how much notice to take of it (most of the time this
//!   flag is ignored).
//! - An archive is the scope a set of cross-references resolves in.
//!   Every file carries an id, and other files reference it by that number
//!   rather than by path.
//! - A lookup by id tries the id as an entry index first O(1), and only searches
//!   for real if that guess misses. That's why a freshly authored archive
//!   numbers every file's id as its own entry index: it keeps every lookup
//!   on the fast path. Unknown why some archives just exist unordered/unoptimized.
//!
//! # Layout
//!
//! Archives are structured as such:
//!
//! ```text
//! 0x00  top header    how big the archive is, and where the data header is
//! 0x20  data header   where each of the four sections below starts
//! 0x40  nodes         one per directory, naming the run of entries it holds
//! ..    entries       one per file and per directory: a file's points at
//!                     its bytes, a directory's at its node
//! ..    string pool   every file and directory name, null terminated
//! ..    file data     the files' bytes, in entry order
//! ```
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

use std::collections::HashSet;

use tpmt_bytes::{Reader, Writer};

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
}

pub type Result<T> = std::result::Result<T, Error>;

/// Which console memory a file is loaded into when its archive is mounted.
///
/// The order the variants are declared in is the order an archive stores them
/// in, and [`pack`] holds callers to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
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

// The pool opens with `.` and `..`, in that order, so the offset every
// directory entry names its own two by is fixed.
const DOT_IN_POOL: u32 = 0x00;
const DOTDOT_IN_POOL: u32 = 0x02;

// Every section starts this aligned, and files' bytes are padded to it.
const ALIGN: usize = 0x20;

/// One archive opened for reading: its bytes, and where each section starts.
///
/// The file states its section offsets relative to the data header. They are
/// resolved to absolute positions once, here, so nothing below has to carry
/// the anchor around.
struct ArchiveReader<'a> {
    /// The whole archive, every read out of it bounds checked.
    reader: Reader<'a>,
    /// One 0x10 record per directory, naming the run of entries it holds.
    nodes_at: usize,
    node_count: usize,
    /// One 0x14 record per file and per directory, `.` and `..` included. A
    /// directory's record points at its node, a file's at its bytes.
    entries_at: usize,
    entry_count: usize,
    /// Every name, null terminated and Shift-JIS, referred to by offset from
    /// the start of it.
    strings_at: usize,
    /// The files' bytes, each padded out to 0x20. Last section in the archive.
    file_data_at: usize,
}

/// Takes an archive apart into every file it holds, directories flattened into
/// the paths. Nothing is copied out of `data`.
///
/// The files come back in the archive's own order, which is the order [`pack`]
/// rebuilds the tree from, so a round trip keeps it.
///
/// Compression flags are dropped, since [`pack`] recomputes them from the
/// file's bytes.
pub fn unpack(data: &[u8]) -> Result<Archive<'_>> {
    if !data.starts_with(top_header::MAGIC) {
        return Err(Error::NotRarc);
    }

    let reader = Reader::new(data);
    if reader.u32_at(top_header::FILE_SIZE)? as usize != data.len() {
        return Err(Error::Corrupt("the stated size is not the actual size"));
    }

    // Helper: resolves an offset field in the data header to a position in
    // the archive. A nonsense offset saturates rather than wrapping to something
    // small, so it stays out of bounds and is caught by the checks below or by
    // the first read that follows it.
    let header = reader.u32_at(top_header::DATA_HEADER_PTR)? as usize;
    let relative = |field| -> Result<usize> {
        Ok(header.saturating_add(reader.u32_at(header + field)? as usize))
    };

    let node_count = reader.u32_at(header + data_header::NODE_COUNT)? as usize;
    let nodes_at = relative(data_header::NODE_LIST_PTR)?;
    let entry_count = reader.u32_at(header + data_header::ENTRY_COUNT)? as usize;
    let entries_at = relative(data_header::ENTRY_LIST_PTR)?;

    // A bad count is refused before it can size an allocation or a walk, so
    // past here the node list and the entry list are known to sit inside the
    // buffer.
    let fits = |offset: usize, count: usize, record: usize| {
        count
            .checked_mul(record)
            .and_then(|len| offset.checked_add(len))
            .is_some_and(|end| end <= data.len())
    };
    if node_count == 0 {
        return Err(Error::Corrupt("there is no root directory"));
    }
    if !fits(nodes_at, node_count, node::LEN) {
        return Err(Error::Corrupt(
            "more directories than the archive could hold",
        ));
    }
    if !fits(entries_at, entry_count, entry::LEN) {
        return Err(Error::Corrupt("more entries than the archive could hold"));
    }

    let opened = ArchiveReader {
        nodes_at,
        node_count,
        entries_at,
        entry_count,
        strings_at: relative(data_header::STRING_POOL_PTR)?,
        file_data_at: header.saturating_add(reader.u32_at(top_header::FILE_DATA_PTR)? as usize),
        reader,
    };
    // The root is node 0, and its name is the one thing read outside the walk.
    let name = opened.reader.u32_at(nodes_at + node::NAME)?;
    let hash = opened.reader.u16_at(nodes_at + node::NAME_HASH)?;
    let root = opened.name(name, hash)?;

    // Only used for the verification below, never again: this is the one
    // place anything reads the stored counter back.
    let stored = opened.reader.u16_at(header + data_header::NEXT_FREE_ID)?;
    let (files, derived) = opened.walk()?;
    Ok(Archive {
        root,
        files,
        next_free_id: (stored != derived).then_some(stored),
    })
}

impl<'a> ArchiveReader<'a> {
    // Flattens the tree into files, depth first from node 0. A directory
    // yields no file of its own, only the prefix its contents go under, so the
    // tree survives in the paths.
    //
    // Hands back the next-free-id counter the files come to, since the ids they
    // came to it under are gone by the time anything else could work it out.
    fn walk(&self) -> Result<(Vec<File<'a>>, u16)> {
        let mut files = Vec::with_capacity(self.entry_count);
        let mut visited = vec![false; self.node_count];
        let mut highest = None;
        let mut synced = true;
        // Each frame is a directory mid-walk: the entries it still owes, and
        // the path prefix its files go under.
        let mut stack = vec![(self.open_node(0, &mut visited)?, String::new())];

        while let Some((range, prefix)) = stack.last_mut() {
            let Some(index) = range.next() else {
                stack.pop();
                continue;
            };

            let record = self.entries_at + index * entry::LEN;
            let flags_and_name = self.reader.u32_at(record + entry::FLAGS_AND_NAME)?;
            let flags = flags_and_name >> entry::FLAGS_SHIFT;
            let hash = self.reader.u16_at(record + entry::NAME_HASH)?;
            let name = self.name(flags_and_name & entry::NAME_MASK, hash)?;

            // Every directory carries a `.` entry pointing at itself and a
            // `..` pointing at its parent, the only link back up.
            // The walk descends with its own stack and needs neither.
            if name == "." || name == ".." {
                continue;
            }
            if name.is_empty() || name.contains(['/', '\\']) {
                return Err(Error::UnusableName(name));
            }

            let path = match prefix.is_empty() {
                true => name,
                false => format!("{prefix}/{name}"),
            };
            let target = self.reader.u32_at(record + entry::DATA_OR_NODE)? as usize;

            if flags & entry::FLAG_DIRECTORY != 0 {
                let range = self.open_node(target, &mut visited)?;
                stack.push((range, path));
            } else {
                // A file's target is the offset of its bytes within the data
                // section, and exactly one of the three memory bits is set.
                let size = self.reader.u32_at(record + entry::DATA_SIZE)? as usize;
                let preload = if flags & entry::FLAG_MRAM != 0 {
                    Preload::Mram
                } else if flags & entry::FLAG_ARAM != 0 {
                    Preload::Aram
                } else if flags & entry::FLAG_DISC != 0 {
                    Preload::Disc
                } else {
                    return Err(Error::Corrupt("a file is marked for no memory at all"));
                };
                let id = self.reader.u16_at(record + entry::ID)?;
                highest = highest.max(Some(id));
                synced &= usize::from(id) == index;
                files.push(File {
                    path,
                    data: self.reader.slice_at(self.file_data_at + target, size)?,
                    id: Some(id),
                    preload,
                });
            }
        }

        Ok((files, next_free_id(self.entry_count, highest, synced)?))
    }

    /// Marks a node visited and hands back the run of entries it owns.
    fn open_node(&self, index: usize, visited: &mut [bool]) -> Result<std::ops::Range<usize>> {
        // A directory aimed at a missing node, or back at an ancestor, cannot
        // happen in a well-formed archive. Neither is stepped over quietly:
        // dropping a subtree here would look exactly like success.
        match visited.get_mut(index) {
            None => {
                return Err(Error::Corrupt(
                    "a directory points at a node that does not exist",
                ));
            }
            Some(true) => return Err(Error::Corrupt("the directory tree loops")),
            Some(seen) => *seen = true,
        }

        let record = self.nodes_at + index * node::LEN;
        let first = self.reader.u32_at(record + node::FIRST_ENTRY)? as usize;
        let count = self.reader.u16_at(record + node::ENTRY_COUNT)? as usize;
        first
            .checked_add(count)
            .filter(|&end| end <= self.entry_count)
            .map(|end| first..end)
            .ok_or(Error::Corrupt(
                "a directory claims entries that do not exist",
            ))
    }

    /// Reads a name out of the string pool. The pool is Shift-JIS: these are
    /// Japanese-authored archives, and a few names are not ASCII.
    ///
    /// Every reference to a name sits beside a hash of it, so an offset that
    /// merely landed on something null-terminated is caught rather than
    /// trusted.
    fn name(&self, offset: u32, hash: u16) -> Result<String> {
        let raw = self.reader.cstr_at(self.strings_at + offset as usize)?;
        if name_hash(raw) != hash {
            return Err(Error::Corrupt("a name does not match its stored hash"));
        }

        let (name, _, malformed) = encoding_rs::SHIFT_JIS.decode(raw);
        match malformed {
            true => Err(Error::Corrupt("a name is not Shift-JIS")),
            false => Ok(name.into_owned()),
        }
    }
}

/// The tree [`pack`] rebuilds from the file paths: directories in the order
/// a walk of the original would have met them, so a round trip lays everything
/// back where it was.
struct Dir {
    /// The name as the pool will store it, encoded once up front.
    name: Vec<u8>,
    parent: usize,
    children: Vec<Child>,
}

enum Child {
    /// Index into the directory list.
    Dir(usize),
    /// Index into the caller's file list.
    File(usize),
}

/// That tree numbered the way the archive stores it: which node each directory
/// becomes, and which run of entries it owns.
struct Tree {
    dirs: Vec<Dir>,
    /// The directories in node order, so `order[node]` is a directory index.
    order: Vec<usize>,
    /// The same thing the other way round: `node_of[dir]` is a node number.
    node_of: Vec<u32>,
    /// Where each node's run of entries begins.
    first_entry: Vec<u32>,
    /// Every entry the archive will hold, each directory's `.` and `..` included.
    entry_count: usize,
}

impl Tree {
    fn build(archive: &Archive) -> Result<Tree> {
        let dirs = grow_dirs(archive)?;

        // Nodes are numbered depth first, children in sibling order. First
        // appearance already walks that way for files that came out of an
        // archive, but nothing forces a caller's list to, so the order is
        // walked out properly here.
        let mut order = Vec::with_capacity(dirs.len());
        let mut node_of = vec![0u32; dirs.len()];
        let mut stack = vec![0];
        while let Some(dir) = stack.pop() {
            node_of[dir] = order.len() as u32;
            order.push(dir);
            for child in dirs[dir].children.iter().rev() {
                if let Child::Dir(sub) = child {
                    stack.push(*sub);
                }
            }
        }

        // Each node owns a run of entries: its children in order, then `.` and
        // `..`. The runs sit in node order, so an entry's index is its node's
        // running total plus its place in the run.
        let mut first_entry = Vec::with_capacity(order.len());
        let mut entry_count: usize = 0;
        for &dir in &order {
            first_entry.push(entry_count as u32);
            entry_count += dirs[dir].children.len() + 2;
        }

        Ok(Tree {
            dirs,
            order,
            node_of,
            first_entry,
            entry_count,
        })
    }

    /// Every file, in the order the entries pointing at it come, which is also
    /// the order the data section is laid out in. Each one arrives as the entry
    /// index it sits at and its index in the caller's file list.
    fn files(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.order.iter().enumerate().flat_map(move |(node, &dir)| {
            let first = self.first_entry[node] as usize;
            self.dirs[dir].children.iter().enumerate().filter_map(
                move |(slot, child)| match child {
                    Child::File(index) => Some((first + slot, *index)),
                    Child::Dir(_) => None,
                },
            )
        })
    }
}

/// Grows the directory list one path at a time. A directory is created by the
/// first file inside it and found by every later one, so sibling order is
/// first-appearance order, which for an unpacked archive is the order the
/// original's entries had.
fn grow_dirs(archive: &Archive) -> Result<Vec<Dir>> {
    let mut dirs = vec![Dir {
        name: encode(&archive.root)?,
        parent: 0,
        children: Vec::new(),
    }];

    for (index, file) in archive.files.iter().enumerate() {
        let mut at = 0;
        let mut parts = file.path.split('/').peekable();
        while let Some(part) = parts.next() {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                return Err(Error::UnusableName(file.path.clone()));
            }
            if parts.peek().is_none() {
                dirs[at].children.push(Child::File(index));
                break;
            }

            let name = encode(part)?;
            at = match dirs[at].children.iter().find_map(|child| match child {
                Child::Dir(dir) if dirs[*dir].name == name => Some(*dir),
                _ => None,
            }) {
                Some(dir) => dir,
                None => {
                    dirs.push(Dir {
                        name,
                        parent: at,
                        children: Vec::new(),
                    });
                    let dir = dirs.len() - 1;
                    dirs[at].children.push(Child::Dir(dir));
                    dir
                }
            };
        }
    }

    Ok(dirs)
}

/// The format's own convention for a fresh id, kept only for reference and
/// not called: a file's own entry index. That's what lets a lookup by id
/// (`JKRArchive::findIdResource` in the decomp) hit its fast path, which
/// tries the id as an entry index before it searches. [`pack`] uses a
/// different rule instead (see there for why): the lowest id not already
/// claimed by another file in the archive.
#[allow(dead_code)]
fn entry_index_fallback(entry: usize) -> Result<u16> {
    u16::try_from(entry).map_err(|_| Error::Oversized)
}

/// Writes a whole archive from its file list.
///
/// Directories only exist as shared prefixes of file paths, so an empty one
/// is dropped: there's no path left to name it. The root is named by the
/// archive itself, so an empty file list still packs.
///
/// Files must arrive grouped by memory: every [`Preload::Mram`] one, then
/// every [`Preload::Aram`] one, then the rest. The header stores one total
/// size per memory rather than tagging each file, so that total is only
/// correct if its group is contiguous; an interleaved list gets
/// [`Error::Ungrouped`] instead of an archive with wrong stated sizes. Path
/// order is unconstrained, and a list straight from [`unpack`] is already
/// grouped.
///
/// Every field is reproduced. A file with no [`File::id`] gets the lowest id
/// no other file in the list already claims, rather than
/// [`entry_index_fallback`]: a partial rebuild can carry ids over from the
/// original, and a fresh one picked by entry index could land on one already
/// in use. This only guards against a collision within the list handed in;
/// it cannot know whether some other file, elsewhere, still references an id
/// that a deleted file used to hold. That is a linker's job once one exists.
/// An [`Archive::next_free_id`] the input didn't carry is derived here too.
pub fn pack(archive: &Archive) -> Result<Vec<u8>> {
    let tree = Tree::build(archive)?;
    let placed = place_files(archive, &tree)?;
    let next_free = match archive.next_free_id {
        Some(stored) => stored,
        None => next_free_id(
            tree.entry_count,
            placed.ids.iter().copied().max(),
            placed.synced,
        )?,
    };
    let pool = build_pool(archive, &tree)?;
    let layout = Layout::of(&tree, pool.bytes.len());

    // Every length is known by now, so the whole archive is one allocation.
    let mut out = Writer::with_capacity(layout.data_at + placed.data_size);
    write_headers(&mut out, &tree, &layout, next_free, placed.synced);
    write_nodes(&mut out, &tree, &pool);
    write_entries(&mut out, archive, &tree, &pool, &placed)?;
    out.bytes(&pool.bytes);
    out.zeros(layout.pool_size - pool.bytes.len());
    write_file_data(&mut out, archive, &tree);

    // The four fields that need the finished file's length.
    let size = u32::try_from(out.len()).map_err(|_| Error::Oversized)?;
    out.u32_at(top_header::FILE_SIZE, size);
    out.u32_at(top_header::TOTAL_DATA_SIZE, size - layout.data_at as u32);
    out.u32_at(top_header::MRAM_SIZE, placed.mram as u32);
    out.u32_at(top_header::ARAM_SIZE, placed.aram as u32);
    Ok(out.finish())
}

/// What a file's entry needs that only a whole pass can say, worked out for
/// every file before the first entry goes out.
struct Placed {
    /// The id each file goes out under, by its place in the caller's list.
    ids: Vec<u16>,
    /// Where each file's bytes land in the data section, same indexing.
    offsets: Vec<u32>,
    /// Whether every file's id came out equal to its entry index.
    synced: bool,
    /// The whole data section, padding included.
    data_size: usize,
    mram: usize,
    aram: usize,
}

/// Works out the id every file goes out under, and where its bytes land in the
/// data section, which is written after the entries pointing into it. Both are
/// walked in the order the entries themselves come, which is the order the data
/// section is laid out in.
///
/// A file without an id claims the lowest one nothing else in the list already
/// holds, so a fallback can never collide with an id carried over from the
/// original (see [`pack`]'s doc, and [`entry_index_fallback`] for the format's
/// own convention). The sync flag records whether every file's final id,
/// however it got one, happens to equal its entry index, the guess a lookup by
/// id tries before it searches, though nothing reads the flag back to decide
/// anything. The two preload totals are the lengths of the runs the game slices
/// the data section into, so they count the padded sizes, and the order check
/// below is what makes them runs rather than sums.
fn place_files(archive: &Archive, tree: &Tree) -> Result<Placed> {
    // Placeholder values: the loop below fills in every slot for real, one per
    // file, before anything reads these back.
    let mut ids = vec![entry::NO_ID; archive.files.len()];
    let mut offsets = vec![0u32; archive.files.len()];
    let mut synced = true;
    let mut data_size: usize = 0;
    let mut mram: usize = 0;
    let mut aram: usize = 0;
    let mut memory = Preload::Mram;

    // Ids already spoken for, so a fallback below never repeats one. Handed
    // out lowest first, so a partial rebuild fills the gaps an id-shuffling
    // caller left rather than jumping past them.
    let claimed: HashSet<u16> = archive.files.iter().filter_map(|file| file.id).collect();
    let mut next_id: u16 = 0;
    let mut claim_id = || -> Result<u16> {
        while claimed.contains(&next_id) || next_id == entry::NO_ID {
            next_id = next_id.checked_add(1).ok_or(Error::Oversized)?;
        }
        let id = next_id;
        next_id = next_id.checked_add(1).ok_or(Error::Oversized)?;
        Ok(id)
    };

    for (entry, index) in tree.files() {
        let file = &archive.files[index];

        // This walk is the data section's own order, so a file that wants an
        // earlier memory than one already written is one the header's two
        // sizes could not describe. Caught here rather than shuffled into
        // place: the fix is the caller's list, and moving files would cost the
        // byte-for-byte rebuild.
        if file.preload < memory {
            return Err(Error::Ungrouped);
        }
        memory = file.preload;

        let entry_id = u16::try_from(entry).map_err(|_| Error::Oversized)?;
        ids[index] = match file.id {
            Some(id) => id,
            None => claim_id()?,
        };
        synced &= ids[index] == entry_id;

        offsets[index] = u32::try_from(data_size).map_err(|_| Error::Oversized)?;
        let padded = file.data.len().next_multiple_of(ALIGN);
        data_size += padded;
        match file.preload {
            Preload::Mram => mram += padded,
            Preload::Aram => aram += padded,
            Preload::Disc => {}
        }
    }

    Ok(Placed {
        ids,
        offsets,
        synced,
        data_size,
        mram,
        aram,
    })
}

/// The string pool, and where in it every name ended up.
struct Pool {
    bytes: Vec<u8>,
    /// Where each directory's name landed, by directory index.
    dirs: Vec<u32>,
    /// Where each file's name landed and what it hashes to, by its place in
    /// the caller's list. The hash rides along because this is where the name
    /// gets encoded, and the entry pass would otherwise have to encode it
    /// again just to hash it. A directory already keeps its encoded name, so
    /// there is nothing to carry for one.
    files: Vec<(u32, u16)>,
}

/// Builds the pool: `.` and `..` once at the front, then, in node order, each
/// directory's name followed by the names of the files in it. A repeated name
/// is stored again, never shared. This is the one section that does not simply
/// follow the node or the entry order, and every retail archive spells it
/// exactly this way.
fn build_pool(archive: &Archive, tree: &Tree) -> Result<Pool> {
    let mut pool = Writer::new();
    pool.bytes(b".\0..\0"); // The two `DOT_IN_POOL` and `DOTDOT_IN_POOL` point at.
    let name_at = |pool: &mut Writer, name: &[u8]| -> Result<u32> {
        let at = u32::try_from(pool.len())
            .ok()
            .filter(|&at| at <= entry::NAME_MASK)
            .ok_or(Error::Oversized)?;
        pool.bytes(name);
        pool.u8(0);
        Ok(at)
    };

    let mut dirs = vec![0u32; tree.dirs.len()];
    let mut files = vec![(0u32, 0u16); archive.files.len()];
    for &dir in &tree.order {
        dirs[dir] = name_at(&mut pool, &tree.dirs[dir].name)?;
        for child in &tree.dirs[dir].children {
            if let Child::File(index) = child {
                let path = &archive.files[*index].path;
                let name = encode(path.rsplit('/').next().unwrap_or(path))?;
                files[*index] = (name_at(&mut pool, &name)?, name_hash(&name));
            }
        }
    }

    Ok(Pool {
        bytes: pool.finish(),
        dirs,
        files,
    })
}

/// Where each section lands, everything after the two headers 0x20 aligned.
struct Layout {
    nodes_at: usize,
    entries_at: usize,
    strings_at: usize,
    /// The pool padded out, which is the size the header states, so the file
    /// data starts exactly where the pool's stated end is.
    pool_size: usize,
    data_at: usize,
}

impl Layout {
    fn of(tree: &Tree, pool_len: usize) -> Layout {
        let nodes_at = data_header::AT + data_header::LEN;
        let entries_at = (nodes_at + tree.order.len() * node::LEN).next_multiple_of(ALIGN);
        let strings_at = (entries_at + tree.entry_count * entry::LEN).next_multiple_of(ALIGN);
        let pool_size = pool_len.next_multiple_of(ALIGN);
        Layout {
            nodes_at,
            entries_at,
            strings_at,
            pool_size,
            data_at: strings_at + pool_size,
        }
    }
}

/// Both headers, which go out as zeros and are then patched field by field.
/// Whatever is never patched stays zero, which is what the unnamed fields hold
/// on a retail archive anyway.
fn write_headers(out: &mut Writer, tree: &Tree, layout: &Layout, next_free: u16, synced: bool) {
    let header = data_header::AT;
    out.bytes(top_header::MAGIC);
    out.zeros(layout.nodes_at - top_header::MAGIC.len());
    out.u32_at(top_header::DATA_HEADER_PTR, header as u32);
    out.u32_at(top_header::FILE_DATA_PTR, (layout.data_at - header) as u32);

    out.u32_at(header + data_header::NODE_COUNT, tree.order.len() as u32);
    out.u32_at(
        header + data_header::NODE_LIST_PTR,
        (layout.nodes_at - header) as u32,
    );
    out.u32_at(header + data_header::ENTRY_COUNT, tree.entry_count as u32);
    out.u32_at(
        header + data_header::ENTRY_LIST_PTR,
        (layout.entries_at - header) as u32,
    );
    out.u32_at(
        header + data_header::STRING_POOL_SIZE,
        layout.pool_size as u32,
    );
    out.u32_at(
        header + data_header::STRING_POOL_PTR,
        (layout.strings_at - header) as u32,
    );
    out.u16_at(header + data_header::NEXT_FREE_ID, next_free);
    out.u8_at(header + data_header::SYNCED_IDS, synced as u8);
}

/// One record per directory, in node order, naming the run of entries it holds.
fn write_nodes(out: &mut Writer, tree: &Tree, pool: &Pool) {
    for (node, &dir) in tree.order.iter().enumerate() {
        // The fourcc: the name ASCII-uppercased, truncated to four, space
        // padded. The root is `ROOT` whatever its name is.
        match node {
            0 => out.bytes(b"ROOT"),
            _ => {
                let mut fourcc = [b' '; 4];
                for (at, byte) in tree.dirs[dir].name.iter().take(4).enumerate() {
                    fourcc[at] = byte.to_ascii_uppercase();
                }
                out.bytes(&fourcc);
            }
        }
        out.u32(pool.dirs[dir]);
        out.u16(name_hash(&tree.dirs[dir].name));
        out.u16((tree.dirs[dir].children.len() + 2) as u16);
        out.u32(tree.first_entry[node]);
    }
    out.align(ALIGN);
}

/// The entries, node by node: a directory's children in order, then its own
/// `.` and `..`. A file's entry is the only place the offsets [`place_files`]
/// worked out are spelled.
fn write_entries(
    out: &mut Writer,
    archive: &Archive,
    tree: &Tree,
    pool: &Pool,
    placed: &Placed,
) -> Result<()> {
    for (node, &dir) in tree.order.iter().enumerate() {
        for child in &tree.dirs[dir].children {
            match child {
                Child::Dir(sub) => {
                    dir_entry(
                        out,
                        &tree.dirs[*sub].name,
                        pool.dirs[*sub],
                        tree.node_of[*sub],
                    );
                }
                Child::File(index) => {
                    let file = &archive.files[*index];
                    let (name, hash) = pool.files[*index];
                    file_entry(
                        out,
                        placed.ids[*index],
                        hash,
                        name,
                        file.preload,
                        placed.offsets[*index],
                        file.data,
                    )?;
                }
            }
        }

        // `.` points at the directory's own node, `..` at its parent's, and
        // the root's `..` at nothing. Both names are at the front of the pool.
        dir_entry(out, b".", DOT_IN_POOL, node as u32);
        let parent = match node {
            0 => u32::MAX,
            _ => tree.node_of[tree.dirs[dir].parent],
        };
        dir_entry(out, b"..", DOTDOT_IN_POOL, parent);
    }
    out.align(ALIGN);
    Ok(())
}

/// The files' bytes last, in the order their entries went out, each padded to
/// the alignment its offset was worked out against.
fn write_file_data(out: &mut Writer, archive: &Archive, tree: &Tree) {
    for (_, index) in tree.files() {
        let data = archive.files[index].data;
        out.bytes(data);
        out.zeros(data.len().next_multiple_of(ALIGN) - data.len());
    }
}

/// One directory's entry: no bytes of its own, pointing at the node it opens
/// rather than at the data section. `name` is what the pool holds at `at`, and
/// is only needed for its hash. Directories share the id that is no id.
fn dir_entry(out: &mut Writer, name: &[u8], at: u32, node: u32) {
    out.u16(entry::NO_ID);
    out.u16(name_hash(name));
    out.u32(entry::FLAG_DIRECTORY << entry::FLAGS_SHIFT | at);
    out.u32(node);
    out.u32(entry::DIRECTORY_SIZE);
    out.u32(0);
}

/// A file's entry: its id, name, memory and compression flags restated from
/// the bytes themselves, and where those bytes and their stored size land.
fn file_entry(
    out: &mut Writer,
    id: u16,
    hash: u16,
    name: u32,
    preload: Preload,
    offset: u32,
    data: &[u8],
) -> Result<()> {
    let size = u32::try_from(data.len()).map_err(|_| Error::Oversized)?;

    // The compression bits restate what the bytes already are.
    let flags = entry::FLAG_FILE
        | match preload {
            Preload::Mram => entry::FLAG_MRAM,
            Preload::Aram => entry::FLAG_ARAM,
            Preload::Disc => entry::FLAG_DISC,
        }
        | match data.starts_with(b"Yaz0") {
            true => entry::FLAG_COMPRESSED | entry::FLAG_YAZ0,
            false => 0,
        };

    out.u16(id);
    out.u16(hash);
    out.u32(flags << entry::FLAGS_SHIFT | name);
    out.u32(offset);
    out.u32(size);
    out.u32(0);
    Ok(())
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

/// A name as the pool stores it. Members decoded from Shift-JIS on the way
/// out have to survive the trip back, and a name the encoding cannot spell
/// has no place an archive could hold it.
fn encode(name: &str) -> Result<Vec<u8>> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(name);
    match unmappable || name.is_empty() {
        true => Err(Error::UnusableName(name.to_owned())),
        false => Ok(bytes.into_owned()),
    }
}

/// The hash stored beside every name reference: each byte folded onto three
/// times the running total.
fn name_hash(name: &[u8]) -> u16 {
    name.iter().fold(0, |hash, &byte| {
        hash.wrapping_mul(3).wrapping_add(byte as u16)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture almost every test opens or corrupts: a root holding `a.bin`
    /// and a subdirectory `sub` holding `b.bin`. It comes out of [`pack`]
    /// rather than being spelled by hand, and `packs_the_retail_layout` below
    /// is what pins that output to the conventions byte by byte, so the two
    /// together are not circular.
    fn fixture() -> Vec<File<'static>> {
        vec![
            File {
                path: "a.bin".into(),
                data: b"AAAAA",
                id: None,
                preload: Preload::Mram,
            },
            File {
                path: "sub/b.bin".into(),
                data: b"BBB",
                id: None,
                preload: Preload::Mram,
            },
        ]
    }

    fn archive() -> Vec<u8> {
        pack(&Archive {
            root: "root".into(),
            files: fixture(),
            ..Default::default()
        })
        .unwrap()
    }

    // Where that fixture's sections land: two headers, two nodes from 0x40,
    // seven entries from 0x60 ending 0xEC, the pool 0x20 aligned after them,
    // and the file data after its padded 0x1A bytes.
    const NODES: usize = 0x40;
    const ENTRIES: usize = 0x60;
    const STRINGS: usize = 0x100;
    const FILE_DATA: usize = 0x120;

    // Positions within the pool `.\0..\0root\0a.bin\0sub\0b.bin\0`.
    const NAME_A: usize = 0x0A;

    fn u16_at(data: &[u8], at: usize) -> u16 {
        u16::from_be_bytes(data[at..at + 2].try_into().unwrap())
    }

    fn u32_at(data: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
    }

    fn put16(data: &mut [u8], at: usize, value: u16) {
        data[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn put32(data: &mut [u8], at: usize, value: u32) {
        data[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }

    /// The whole fixture image checked field by field against the layout the
    /// retail discs use. Everything else in this module trusts `pack`, and
    /// this is what earns that: any drift in the conventions lands here.
    #[test]
    fn packs_the_retail_layout() {
        let data = archive();
        assert_eq!(data.len(), 0x160);

        assert_eq!(&data[..4], b"RARC");
        assert_eq!(u32_at(&data, top_header::FILE_SIZE), 0x160);
        assert_eq!(u32_at(&data, top_header::DATA_HEADER_PTR), 0x20);
        assert_eq!(u32_at(&data, top_header::FILE_DATA_PTR), 0x100);
        assert_eq!(u32_at(&data, top_header::TOTAL_DATA_SIZE), 0x40);
        assert_eq!(u32_at(&data, top_header::MRAM_SIZE), 0x40);
        assert_eq!(u32_at(&data, top_header::ARAM_SIZE), 0);
        // The unnamed tail of the top header, zero here as on the discs.
        assert_eq!(u32_at(&data, 0x1C), 0);

        assert_eq!(u32_at(&data, data_header::AT + data_header::NODE_COUNT), 2);
        assert_eq!(
            u32_at(&data, data_header::AT + data_header::NODE_LIST_PTR),
            0x20
        );
        assert_eq!(u32_at(&data, data_header::AT + data_header::ENTRY_COUNT), 7);
        assert_eq!(
            u32_at(&data, data_header::AT + data_header::ENTRY_LIST_PTR),
            0x40
        );
        assert_eq!(
            u32_at(&data, data_header::AT + data_header::STRING_POOL_SIZE),
            0x20
        );
        assert_eq!(
            u32_at(&data, data_header::AT + data_header::STRING_POOL_PTR),
            0xE0
        );
        assert_eq!(
            u16_at(&data, data_header::AT + data_header::NEXT_FREE_ID),
            2
        );
        assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 0);

        // The root is `ROOT` whatever its name; other nodes uppercase theirs.
        assert_eq!(&data[NODES..NODES + 4], b"ROOT");
        assert_eq!(u32_at(&data, NODES + node::NAME), 5);
        assert_eq!(u16_at(&data, NODES + node::NAME_HASH), name_hash(b"root"));
        assert_eq!(u16_at(&data, NODES + node::ENTRY_COUNT), 4);
        assert_eq!(u32_at(&data, NODES + node::FIRST_ENTRY), 0);
        let sub = NODES + node::LEN;
        assert_eq!(&data[sub..sub + 4], b"SUB ");
        assert_eq!(u32_at(&data, sub + node::NAME), 16);
        assert_eq!(u16_at(&data, sub + node::ENTRY_COUNT), 3);
        assert_eq!(u32_at(&data, sub + node::FIRST_ENTRY), 4);

        // Root's entries: `a.bin`, `sub`, then `.` and `..` last, the order
        // every retail directory uses. Directories share the id that is no
        // id, and the root's `..` points at nothing. Ids are handed out
        // lowest first as files are met, not by entry index: a.bin claims 0,
        // and b.bin claims 1 rather than the 4 its entry sits at.
        let entry = |index: usize| {
            let at = ENTRIES + index * entry::LEN;
            (
                u16_at(&data, at),
                u32_at(&data, at + entry::FLAGS_AND_NAME),
                u32_at(&data, at + entry::DATA_OR_NODE),
                u32_at(&data, at + entry::DATA_SIZE),
            )
        };
        assert_eq!(entry(0), (0, 0x11 << 24 | 10, 0, 5));
        assert_eq!(entry(1), (0xFFFF, 0x02 << 24 | 16, 1, 0x10));
        assert_eq!(entry(2), (0xFFFF, 0x02 << 24, 0, 0x10));
        assert_eq!(entry(3), (0xFFFF, 0x02 << 24 | 2, u32::MAX, 0x10));
        assert_eq!(entry(4), (1, 0x11 << 24 | 20, 0x20, 3));
        assert_eq!(entry(5), (0xFFFF, 0x02 << 24, 1, 0x10));
        assert_eq!(entry(6), (0xFFFF, 0x02 << 24 | 2, 0, 0x10));
        assert_eq!(
            u16_at(&data, ENTRIES + entry::NAME_HASH),
            name_hash(b"a.bin")
        );

        // The pool: dots once up front, then each directory's name followed by
        // its files' names, zero padded out to alignment.
        assert_eq!(
            &data[STRINGS..FILE_DATA],
            b".\0..\0root\0a.bin\0sub\0b.bin\0\0\0\0\0\0\0"
        );

        // Member bytes in entry order, each padded out to 0x20.
        assert_eq!(&data[FILE_DATA..FILE_DATA + 5], b"AAAAA");
        assert_eq!(&data[FILE_DATA + 0x20..FILE_DATA + 0x23], b"BBB");
        assert!(data[FILE_DATA + 0x23..].iter().all(|&byte| byte == 0));
    }

    /// The fidelity contract: what comes out goes back in and reproduces the
    /// bytes, and what was packed reads back as it was given.
    ///
    /// Both ids come back as they were stored: 0 and 1, the order the fixture's
    /// two files were met in, not the entry indices they sit at.
    #[test]
    fn round_trips_byte_for_byte() {
        let data = archive();
        let opened = unpack(&data).unwrap();
        assert_eq!(opened.root, "root");

        let listed: Vec<_> = opened
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.data, file.id, file.preload))
            .collect();
        assert_eq!(
            listed,
            [
                ("a.bin", b"AAAAA".as_slice(), Some(0), Preload::Mram),
                ("sub/b.bin", b"BBB".as_slice(), Some(1), Preload::Mram),
            ]
        );

        assert_eq!(pack(&opened).unwrap(), data);
    }

    #[test]
    fn an_empty_archive_still_has_its_root() {
        let data = pack(&Archive {
            root: "bmgres99".into(),
            files: Vec::new(),
            ..Default::default()
        })
        .unwrap();
        let opened = unpack(&data).unwrap();
        assert_eq!(opened.root, "bmgres99");
        assert!(opened.files.is_empty());
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// Nodes are numbered depth first, children in sibling order, so `a` and
    /// everything under it is numbered before `b` is reached. Breadth first
    /// would have put both `sub` directories after `b`, and the two `sub`s are
    /// separate directories because a name is only ever looked up among one
    /// parent's children.
    #[test]
    fn nested_directories_are_numbered_depth_first() {
        let files: Vec<_> = [
            ("a/x.bin", b"X"),
            ("a/sub/y.bin", b"Y"),
            ("b/sub/z.bin", b"Z"),
        ]
        .iter()
        .map(|(path, data)| File {
            path: (*path).into(),
            data: data.as_slice(),
            ..Default::default()
        })
        .collect();
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();

        // Five directories, and the runs of entries they own: each node's
        // children, then its own `.` and `..`, laid out in node order.
        assert_eq!(u32_at(&data, data_header::AT + data_header::NODE_COUNT), 5);
        assert_eq!(
            u32_at(&data, data_header::AT + data_header::ENTRY_COUNT),
            17
        );

        let nodes = data_header::AT + data_header::LEN;
        let node = |index: usize| {
            let at = nodes + index * node::LEN;
            (
                &data[at..at + 4],
                u16_at(&data, at + node::ENTRY_COUNT),
                u32_at(&data, at + node::FIRST_ENTRY),
            )
        };
        assert_eq!(node(0), (b"ROOT".as_slice(), 4, 0));
        assert_eq!(node(1), (b"A   ".as_slice(), 4, 4));
        assert_eq!(node(2), (b"SUB ".as_slice(), 3, 8));
        assert_eq!(node(3), (b"B   ".as_slice(), 3, 11));
        assert_eq!(node(4), (b"SUB ".as_slice(), 3, 14));

        // A nested directory's `..` names its own parent rather than the root:
        // it is the last entry of its run, and both `sub` nodes have one.
        let entries =
            data_header::AT + u32_at(&data, data_header::AT + data_header::ENTRY_LIST_PTR) as usize;
        let parent_of =
            |entry: usize| u32_at(&data, entries + entry * entry::LEN + entry::DATA_OR_NODE);
        assert_eq!(parent_of(10), 1);
        assert_eq!(parent_of(16), 3);

        // Which is the tree the paths come back as, `z.bin` under `b/sub`
        // rather than under the `sub` that already existed.
        let opened = unpack(&data).unwrap();
        let paths: Vec<_> = opened.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["a/x.bin", "a/sub/y.bin", "b/sub/z.bin"]);
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// A flat archive comes out numbered the way the format's own convention
    /// would have numbered it, every file's id its own entry index, so the sync
    /// flag goes out set and the counter counts entries rather than ids. Both
    /// ends derive that counter the same way, which is what leaves nothing for
    /// [`Archive::next_free_id`] to carry.
    #[test]
    fn ids_that_match_their_entries_set_the_sync_flag() {
        let files = vec![
            File {
                path: "a.bin".into(),
                data: b"AAAAA",
                ..Default::default()
            },
            File {
                path: "b.bin".into(),
                data: b"BBB",
                ..Default::default()
            },
        ];
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();

        assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 1);
        // The two files, then the root's `.` and `..`.
        assert_eq!(u32_at(&data, data_header::AT + data_header::ENTRY_COUNT), 4);
        assert_eq!(
            u16_at(&data, data_header::AT + data_header::NEXT_FREE_ID),
            4
        );

        let opened = unpack(&data).unwrap();
        assert_eq!(opened.files[0].id, Some(0));
        assert_eq!(opened.files[1].id, Some(1));
        assert!(opened.next_free_id.is_none());
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// A carried id that no longer matches its entry index clears the sync
    /// flag, and the next-free counter follows the ids instead of the count.
    #[test]
    fn carried_ids_clear_the_sync_flag() {
        let mut files = fixture();
        files[1].id = Some(9);
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 0);
        assert_eq!(
            u16_at(&data, data_header::AT + data_header::NEXT_FREE_ID),
            10
        );
        assert_eq!(unpack(&data).unwrap().files[1].id, Some(9));
    }

    /// A fallback id must never repeat one the caller already carried over,
    /// even one that happens to sit at another file's entry index: `a.bin`
    /// takes the id `sub/b.bin`'s entry index would otherwise have handed
    /// out, so the fallback has to look elsewhere and lands on 0, the lowest
    /// id nothing else claims.
    #[test]
    fn a_fallback_id_never_collides_with_a_carried_one() {
        let mut files = fixture();
        files[0].id = Some(4);
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();
        let opened = unpack(&data).unwrap();
        assert_eq!(opened.files[0].id, Some(4));
        assert_eq!(opened.files[1].id, Some(0));
    }

    /// A counter that is not what the ids come to is the archive's own, and a
    /// few were stored that way. Nothing else can bring it back, so it is
    /// carried, and a counter that is what they come to is not.
    #[test]
    fn a_counter_of_its_own_is_carried() {
        let mut data = archive();
        assert!(unpack(&data).unwrap().next_free_id.is_none());

        let derived = u16_at(&data, data_header::AT + data_header::NEXT_FREE_ID);
        let stored = derived + 3;
        data[data_header::AT + data_header::NEXT_FREE_ID
            ..data_header::AT + data_header::NEXT_FREE_ID + 2]
            .copy_from_slice(&stored.to_be_bytes());

        let opened = unpack(&data).unwrap();
        assert_eq!(opened.next_free_id, Some(stored));
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// The compression bits restate what the file's bytes are, so Yaz0 data
    /// is marked compressed no matter what the caller thinks it is.
    #[test]
    fn compression_bits_follow_the_file_bytes() {
        let mut files = fixture();
        files[0].data = b"Yaz0 in shape only";
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(data[ENTRIES + entry::FLAGS_AND_NAME], 0x95);
    }

    /// The two sizes cover one run each, so they only mean anything with the
    /// memories in order. Here the ARAM file is the later of the two, which
    /// puts its bytes second, exactly what the sizes then claim.
    #[test]
    fn preload_totals_split_by_memory() {
        let mut files = fixture();
        files[1].preload = Preload::Aram;
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(u32_at(&data, top_header::MRAM_SIZE), 0x20);
        assert_eq!(u32_at(&data, top_header::ARAM_SIZE), 0x20);
        assert_eq!(unpack(&data).unwrap().files[1].preload, Preload::Aram);
    }

    /// Turn that list around and there is no pair of sizes that describes it,
    /// so it is refused instead of packed into something the game would slice
    /// down the middle of a file.
    #[test]
    fn refuses_files_out_of_memory_order() {
        let mut files = fixture();
        files[0].preload = Preload::Aram;
        let result = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        });
        assert!(matches!(result, Err(Error::Ungrouped)));
    }

    /// The order checked is the data section's, not the file list's, and the
    /// two come apart: entries go out directory by directory, so `a.bin` is
    /// written before `sub/b.bin` however the list is arranged. Marking
    /// `a.bin` for ARAM is out of order despite it being second in the list,
    /// and marking `sub/b.bin` is in order despite it being first.
    #[test]
    fn memory_order_follows_the_data_section() {
        let listed = vec![fixture()[1].clone(), fixture()[0].clone()];

        let mut files = listed.clone();
        files[1].preload = Preload::Aram;
        assert!(matches!(
            pack(&Archive {
                root: "root".into(),
                files,
                ..Default::default()
            }),
            Err(Error::Ungrouped)
        ));

        let mut files = listed;
        files[0].preload = Preload::Aram;
        let data = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(u32_at(&data, top_header::MRAM_SIZE), 0x20);
        assert_eq!(u32_at(&data, top_header::ARAM_SIZE), 0x20);
    }

    #[test]
    fn decodes_shift_jis_names() {
        let mut data = archive();
        // Halfwidth katakana RI, one byte in Shift-JIS.
        data[STRINGS + NAME_A] = 0xD8;
        put16(
            &mut data,
            ENTRIES + entry::NAME_HASH,
            name_hash(b"\xD8.bin"),
        );
        let opened = unpack(&data).unwrap();
        assert_eq!(opened.files[0].path, "ﾘ.bin");
        // And the trip back spells it in Shift-JIS again.
        assert_eq!(pack(&opened).unwrap(), data);
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(unpack(b"Yaz0...."), Err(Error::NotRarc)));
    }

    /// A truncated archive no longer matches its own size field, and is turned
    /// away before any record read walks off the end.
    #[test]
    fn rejects_a_truncated_archive() {
        let data = archive();
        assert!(matches!(
            unpack(&data[..data.len() - 4]),
            Err(Error::Corrupt(_))
        ));
    }

    /// An archive claiming no directories at all has nothing the walk could
    /// start from, and says so rather than complaining about node 0 missing
    /// once it gets there. The message is what the match names: without the
    /// check up front the walk refuses this archive too, so matching only the
    /// variant would pass either way.
    #[test]
    fn rejects_an_archive_with_no_nodes() {
        let mut data = archive();
        put32(&mut data, data_header::AT + data_header::NODE_COUNT, 0);
        assert!(matches!(
            unpack(&data),
            Err(Error::Corrupt("there is no root directory"))
        ));
    }

    /// Either nonsense count is caught up front, before the walk takes it as a
    /// vector length. The complaint is matched too, since a count let through
    /// here is still refused later, only after that allocation.
    #[test]
    fn rejects_counts_that_cannot_fit() {
        let counts = [
            (
                data_header::NODE_COUNT,
                "more directories than the archive could hold",
            ),
            (
                data_header::ENTRY_COUNT,
                "more entries than the archive could hold",
            ),
        ];
        for (field, complaint) in counts {
            let mut data = archive();
            put32(&mut data, data_header::AT + field, u32::MAX);
            assert!(
                matches!(unpack(&data), Err(Error::Corrupt(message)) if message == complaint),
                "{complaint}"
            );
        }
    }

    #[test]
    fn rejects_a_directory_claiming_missing_entries() {
        let mut data = archive();
        put16(&mut data, NODES + node::ENTRY_COUNT, 100);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    /// A cycle marks the archive corrupt rather than producing a partial
    /// listing.
    #[test]
    fn a_directory_cycle_is_refused() {
        let mut data = archive();
        // Aim `sub`'s entry back at the root's node.
        put32(&mut data, ENTRIES + entry::LEN + entry::DATA_OR_NODE, 0);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_dangling_directory_is_refused() {
        let mut data = archive();
        put32(&mut data, ENTRIES + entry::LEN + entry::DATA_OR_NODE, 9);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_name_with_a_separator() {
        let mut data = archive();
        data[STRINGS + NAME_A + 1] = b'/';
        put16(&mut data, ENTRIES + entry::NAME_HASH, name_hash(b"a/bin"));
        assert!(matches!(unpack(&data), Err(Error::UnusableName(_))));
    }

    #[test]
    fn rejects_a_name_that_is_not_shift_jis() {
        let mut data = archive();
        // A lead byte with no trail byte after it.
        data[STRINGS + NAME_A] = 0x85;
        put16(
            &mut data,
            ENTRIES + entry::NAME_HASH,
            name_hash(b"\x85.bin"),
        );
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_wrong_name_hash() {
        let mut data = archive();
        put16(&mut data, ENTRIES + entry::NAME_HASH, 0xBEEF);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_file_marked_for_no_memory() {
        let mut data = archive();
        data[ENTRIES + entry::FLAGS_AND_NAME] = 0x01;
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    /// A path that could climb, or a component nothing could be named by, is
    /// refused rather than packed into something the game would misread.
    #[test]
    fn refuses_an_unusable_path() {
        for path in ["sub/../b.bin", "", "sub//b.bin", "."] {
            let files = vec![File {
                path: path.into(),
                data: b"",
                id: None,
                preload: Preload::Mram,
            }];
            let result = pack(&Archive {
                root: "root".into(),
                files,
                ..Default::default()
            });
            assert!(matches!(result, Err(Error::UnusableName(_))), "{path}");
        }
    }
}
