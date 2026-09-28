//! The write path: turns an [`Archive`] back into bytes.

use std::collections::HashSet;

use tpmt_bytes::{Be16, Be32, Flag, Writer};

use crate::{
    Archive, Error, Preload, Result,
    data_header::{self, DataHeader},
    entry, name_hash, next_free_id,
    node::{self, Node},
    top_header::{self, TopHeader},
};

// The string pool opens with `.` and `..`, in that order, so the offset
// every directory entry names its own two by is fixed.
const DOT_IN_STRING_POOL: u32 = 0x00;
const DOTDOT_IN_STRING_POOL: u32 = 0x02;

// Every section starts this aligned, and files' bytes are padded to it.
const ALIGN: usize = 0x20;

/// One directory.
/// [`grow_dirs`] builds a list of these from the file paths, and
/// [`DirTree`] then numbers that list the way the archive stores it.
struct Dir {
    /// The name as the pool will store it, encoded once up front.
    name: Vec<u8>,
    parent: usize,
    children: Vec<Entry>,
}

/// One thing a [`Dir`] holds directly under it.
enum Entry {
    /// A subdirectory, by index into the directory list.
    Dir(usize),
    /// A file, by index into the caller's file list.
    File(usize),
}

/// Every [`Dir`] brought together into the tree they form, plus the
/// numbering the archive stores them under: which node each becomes, and
/// which run of entries it owns.
struct DirTree {
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

impl DirTree {
    /// Numbers the raw shape [`grow_dirs`] grew: which node each directory
    /// becomes, and which run of entries it owns.
    fn build(archive: &Archive) -> Result<Self> {
        let dirs = grow_dirs(archive)?;

        // Nodes are numbered depth first, children in sibling order. First
        // appearance already walks that way for files that came out of an
        // archive, but nothing forces a caller's list to, so the order is
        // walked out properly here.
        let mut order = Vec::with_capacity(dirs.len());
        let mut node_of = vec![0u32; dirs.len()];
        let mut stack = vec![0];
        while let Some(dir) = stack.pop() {
            node_of[dir] = u32::try_from(order.len()).map_err(|_| Error::Oversized)?;
            order.push(dir);
            for child in dirs[dir].children.iter().rev() {
                if let Entry::Dir(sub) = child {
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
            first_entry.push(u32::try_from(entry_count).map_err(|_| Error::Oversized)?);
            entry_count += dirs[dir].children.len() + 2;
        }

        Ok(Self {
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
                    Entry::File(index) => Some((first + slot, *index)),
                    Entry::Dir(_) => None,
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
                dirs[at].children.push(Entry::File(index));
                break;
            }

            let name = encode(part)?;
            at = dirs[at]
                .children
                .iter()
                .find_map(|child| match child {
                    Entry::Dir(dir) if dirs[*dir].name == name => Some(*dir),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    dirs.push(Dir {
                        name,
                        parent: at,
                        children: Vec::new(),
                    });
                    let dir = dirs.len() - 1;
                    dirs[at].children.push(Entry::Dir(dir));
                    dir
                });
        }
    }

    Ok(dirs)
}

pub fn pack(archive: &Archive) -> Result<Vec<u8>> {
    // Numbers every directory and file; everything below is keyed off that.
    let tree = DirTree::build(archive)?;
    // Only needs the tree's numbering, not the string pool built below.
    let placed = place_files(archive, &tree)?;
    let next_free = match archive.next_free_id {
        Some(stored) => stored,
        None => next_free_id(
            tree.entry_count,
            placed.ids.iter().copied().max(),
            placed.synced,
        )?,
    };
    // Also only needs the numbering, independent of `placed`.
    let string_pool = build_string_pool(archive, &tree)?;
    // Needs the finished pool's length, so it goes last.
    let sections = SectionOffsets::of(&tree, string_pool.bytes.len());

    // Every length is known by now, so the whole archive is one allocation.
    let mut out = Writer::with_capacity(sections.data_at + placed.data_size);
    write_headers(&mut out, &tree, &sections, &placed, next_free)?;
    write_nodes(&mut out, &tree, &string_pool)?;
    write_entries(&mut out, archive, &tree, &string_pool, &placed)?;
    out.bytes(&string_pool.bytes);
    out.zeros(sections.string_pool_size - string_pool.bytes.len());
    write_file_data(&mut out, archive, &tree);
    debug_assert_eq!(out.len(), sections.data_at + placed.data_size);
    Ok(out.finish())
}

/// Where each file's raw bytes get placed in the data section, plus the id
/// it goes out under.
struct Placement {
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
/// original (see `Archive::encode`'s doc, and the comment below for the format's own
/// convention). The `synced` flag records whether every file's final id,
/// however it got one, happens to equal its entry index, the guess a lookup by
/// id tries before it searches, though nothing reads the flag back to decide
/// anything. The two preload totals are the lengths of the runs the game slices
/// the data section into, so they count the padded sizes, and the order check
/// below is what makes them runs rather than sums.
//
// The format's own convention, for reference: a fresh file gets the
// header's next-free-id counter, which only ever advances and never
// reclaims a removed file's id. That's why an archive where every id
// equals its entry index lets a lookup by id (`JKRArchive::findIdResource`
// in the decomp) try the id as an entry index before it searches.
//
// We use a different rule below: the lowest id nothing else in the list
// claims. Filling gaps keeps the id space bounded by what's in use, and
// incidentally still drifts a rebuilt archive back toward that fast path
// rather than away from it. Because of this, next-free-id is not needed
// and is purely bookkeeping.
fn place_files(archive: &Archive, tree: &DirTree) -> Result<Placement> {
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

    Ok(Placement {
        ids,
        offsets,
        synced,
        data_size,
        mram,
        aram,
    })
}

/// The string pool, and where in it every name ended up.
struct StringPool {
    bytes: Vec<u8>,
    /// Where each directory's name landed, by directory index.
    dir_name_ats: Vec<u32>,
    /// Where each file's name landed and what it hashes to, by its place in
    /// the caller's list. The hash rides along because this is where the name
    /// gets encoded, and the entry pass would otherwise have to encode it
    /// again just to hash it. A directory already keeps its encoded name, so
    /// there is nothing to carry for one.
    file_name_ats: Vec<(u32, u16)>,
}

/// Builds the pool: `.` and `..` once at the front, then, in node order, each
/// directory's name followed by the names of the files in it. A repeated name
/// is stored again, never shared. This is the one section that does not simply
/// follow the node or the entry order, and every retail archive spells it
/// exactly this way.
fn build_string_pool(archive: &Archive, tree: &DirTree) -> Result<StringPool> {
    let mut pool = Writer::new();
    pool.bytes(b".\0..\0"); // The two `DOT_IN_STRING_POOL` and `DOTDOT_IN_STRING_POOL` point at.
    let name_at = |pool: &mut Writer, name: &[u8]| -> Result<u32> {
        let at = u32::try_from(pool.len())
            .ok()
            .filter(|&at| at <= entry::NAME_MASK)
            .ok_or(Error::Oversized)?;
        pool.bytes(name);
        pool.u8(0);
        Ok(at)
    };

    let mut dir_name_ats = vec![0u32; tree.dirs.len()];
    let mut file_name_ats = vec![(0u32, 0u16); archive.files.len()];
    for &dir in &tree.order {
        dir_name_ats[dir] = name_at(&mut pool, &tree.dirs[dir].name)?;
        for child in &tree.dirs[dir].children {
            if let Entry::File(index) = child {
                let path = &archive.files[*index].path;
                let name = encode(path.rsplit('/').next().unwrap_or(path))?;
                file_name_ats[*index] = (name_at(&mut pool, &name)?, name_hash(&name));
            }
        }
    }

    Ok(StringPool {
        bytes: pool.finish(),
        dir_name_ats,
        file_name_ats,
    })
}

/// Where each section lands, everything after the two headers 0x20 aligned.
struct SectionOffsets {
    nodes_at: usize,
    entries_at: usize,
    string_pool_at: usize,
    /// The pool padded out, which is the size the header states, so the file
    /// data starts exactly where the pool's stated end is.
    string_pool_size: usize,
    data_at: usize,
}

impl SectionOffsets {
    const fn of(tree: &DirTree, string_pool_len: usize) -> Self {
        let nodes_at = data_header::AT + data_header::LEN;
        let entries_at = (nodes_at + tree.order.len() * node::LEN).next_multiple_of(ALIGN);
        let string_pool_at = (entries_at + tree.entry_count * entry::LEN).next_multiple_of(ALIGN);
        let string_pool_size = string_pool_len.next_multiple_of(ALIGN);
        Self {
            nodes_at,
            entries_at,
            string_pool_at,
            string_pool_size,
            data_at: string_pool_at + string_pool_size,
        }
    }
}

/// Writes the top header and the data header.
fn write_headers(
    out: &mut Writer,
    tree: &DirTree,
    sections: &SectionOffsets,
    placed: &Placement,
    next_free: u16,
) -> Result<()> {
    let be32 = |value: usize| {
        u32::try_from(value)
            .map(Be32::new)
            .map_err(|_| Error::Oversized)
    };
    let header = data_header::AT;
    out.record(&TopHeader {
        magic: *top_header::MAGIC,
        file_size: be32(sections.data_at + placed.data_size)?,
        data_header_ptr: be32(header)?,
        file_data_ptr: be32(sections.data_at - header)?,
        total_data_size: be32(placed.data_size)?,
        mram_size: be32(placed.mram)?,
        aram_size: be32(placed.aram)?,
        unnamed: [0; 4],
    });
    out.record(&DataHeader {
        node_count: be32(tree.order.len())?,
        node_list_ptr: be32(sections.nodes_at - header)?,
        entry_count: be32(tree.entry_count)?,
        entry_list_ptr: be32(sections.entries_at - header)?,
        string_pool_size: be32(sections.string_pool_size)?,
        string_pool_ptr: be32(sections.string_pool_at - header)?,
        next_free_id: Be16::new(next_free),
        synced_ids: Flag::new(placed.synced),
        unnamed: [0; 5],
    });
    Ok(())
}

/// One record per directory, in node order, naming the run of entries it holds.
fn write_nodes(out: &mut Writer, tree: &DirTree, string_pool: &StringPool) -> Result<()> {
    for (node, &dir) in tree.order.iter().enumerate() {
        // The fourcc: the name ASCII-uppercased, truncated to four, space
        // padded. The root is `ROOT` whatever its name is.
        let mut tag = *b"ROOT";
        if node != 0 {
            tag = [b' '; 4];
            for (slot, byte) in tag.iter_mut().zip(&tree.dirs[dir].name) {
                *slot = byte.to_ascii_uppercase();
            }
        }
        out.record(&Node {
            tag,
            name: Be32::new(string_pool.dir_name_ats[dir]),
            name_hash: Be16::new(name_hash(&tree.dirs[dir].name)),
            entry_count: Be16::new(
                u16::try_from(tree.dirs[dir].children.len() + 2).map_err(|_| Error::Oversized)?,
            ),
            first_entry: Be32::new(tree.first_entry[node]),
        });
    }
    out.align(ALIGN);
    Ok(())
}

/// The entries, node by node: a directory's children in order, then its own
/// `.` and `..`. A file's entry is the only place the offsets [`place_files`]
/// worked out are spelled.
fn write_entries(
    out: &mut Writer,
    archive: &Archive,
    tree: &DirTree,
    string_pool: &StringPool,
    placed: &Placement,
) -> Result<()> {
    for (node, &dir) in tree.order.iter().enumerate() {
        for child in &tree.dirs[dir].children {
            match child {
                Entry::Dir(sub) => {
                    dir_entry(
                        out,
                        &tree.dirs[*sub].name,
                        string_pool.dir_name_ats[*sub],
                        tree.node_of[*sub],
                    );
                }
                Entry::File(index) => {
                    let file = &archive.files[*index];
                    let (name_at, hash) = string_pool.file_name_ats[*index];
                    file_entry(
                        out,
                        &StoredEntry {
                            id: placed.ids[*index],
                            hash,
                            name_at,
                            offset: placed.offsets[*index],
                        },
                        file.preload,
                        file.data,
                    )?;
                }
            }
        }

        // `.` points at the directory's own node, `..` at its parent's, and
        // the root's `..` at nothing. Both names are at the front of the pool.
        dir_entry(
            out,
            b".",
            DOT_IN_STRING_POOL,
            u32::try_from(node).map_err(|_| Error::Oversized)?,
        );
        let parent = match node {
            0 => u32::MAX,
            _ => tree.node_of[tree.dirs[dir].parent],
        };
        dir_entry(out, b"..", DOTDOT_IN_STRING_POOL, parent);
    }
    out.align(ALIGN);
    Ok(())
}

/// The files' bytes last, in the order their entries went out, each padded to
/// the alignment its offset was worked out against.
fn write_file_data(out: &mut Writer, archive: &Archive, tree: &DirTree) {
    for (_, index) in tree.files() {
        let data = archive.files[index].data;
        out.bytes(data);
        out.zeros(data.len().next_multiple_of(ALIGN) - data.len());
    }
}

/// One directory's entry: no bytes of its own, pointing at the node it opens
/// rather than at the data section. `name` is what the pool holds at
/// `name_at`, and is only needed for its hash. Directories share the id that
/// is no id.
fn dir_entry(out: &mut Writer, name: &[u8], name_at: u32, node: u32) {
    out.record(&entry::Entry {
        id: Be16::new(entry::NO_ID),
        name_hash: Be16::new(name_hash(name)),
        flags_and_name: Be32::new(entry::FLAG_DIRECTORY << entry::FLAGS_SHIFT | name_at),
        data_or_node: Be32::new(node),
        data_size: Be32::new(entry::DIRECTORY_SIZE),
        unnamed: [0; 4],
    });
}

/// The four fields a file's entry states about itself that nothing else on
/// the entry (its memory or its bytes) already says.
struct StoredEntry {
    id: u16,
    hash: u16,
    name_at: u32,
    offset: u32,
}

/// A file's entry: its id, name, memory and compression flags restated from
/// the bytes themselves, and where those bytes and their stored size land.
fn file_entry(out: &mut Writer, entry: &StoredEntry, preload: Preload, data: &[u8]) -> Result<()> {
    let size = u32::try_from(data.len()).map_err(|_| Error::Oversized)?;

    // The compression bits restate what the bytes already are.
    let flags = entry::FLAG_FILE
        | match preload {
            Preload::Mram => entry::FLAG_MRAM,
            Preload::Aram => entry::FLAG_ARAM,
            Preload::Disc => entry::FLAG_DISC,
        }
        | if data.starts_with(b"Yaz0") {
            entry::FLAG_COMPRESSED | entry::FLAG_YAZ0
        } else {
            0
        };

    out.record(&entry::Entry {
        id: Be16::new(entry.id),
        name_hash: Be16::new(entry.hash),
        flags_and_name: Be32::new(flags << entry::FLAGS_SHIFT | entry.name_at),
        data_or_node: Be32::new(entry.offset),
        data_size: Be32::new(size),
        unnamed: [0; 4],
    });
    Ok(())
}

/// A name as the pool stores it. Members decoded from Shift-JIS on the way
/// out have to survive the trip back, and a name the encoding cannot spell
/// has no place an archive could hold it.
fn encode(name: &str) -> Result<Vec<u8>> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(name);
    if unmappable || name.is_empty() {
        Err(Error::UnusableName(name.to_owned()))
    } else {
        Ok(bytes.into_owned())
    }
}

#[cfg(test)]
pub mod tests {
    use std::mem::offset_of;

    use tpmt_bytes::Reader;

    use super::*;
    use crate::unpack::unpack;
    use crate::{File, entry::Entry};

    pub fn fixture() -> Vec<File<'static>> {
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

    pub fn archive() -> Vec<u8> {
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
    pub const NODES: usize = 0x40;
    pub const ENTRIES: usize = 0x60;
    pub const STRINGS: usize = 0x100;
    pub const FILE_DATA: usize = 0x120;

    // Positions within the pool `.\0..\0root\0a.bin\0sub\0b.bin\0`.
    pub const NAME_A: usize = 0x0A;

    /// The whole fixture image checked field by field against the layout the
    /// retail discs use. Everything else in this module trusts `pack`, and
    /// this is what earns that: any drift in the conventions lands here.
    #[test]
    fn packs_the_retail_layout() {
        let data = archive();
        let r = Reader::new(&data);
        assert_eq!(data.len(), 0x160);

        let top: &TopHeader = r.view_at(0).unwrap();
        assert_eq!(&top.magic, b"RARC");
        assert_eq!(top.file_size.get(), 0x160);
        assert_eq!(top.data_header_ptr.get(), 0x20);
        assert_eq!(top.file_data_ptr.get(), 0x100);
        assert_eq!(top.total_data_size.get(), 0x40);
        assert_eq!(top.mram_size.get(), 0x40);
        assert_eq!(top.aram_size.get(), 0);
        // The unnamed tail of the top header, zero here as on the discs.
        assert_eq!(top.unnamed, [0; 4]);

        let header: &DataHeader = r.view_at(data_header::AT).unwrap();
        assert_eq!(header.node_count.get(), 2);
        assert_eq!(header.node_list_ptr.get(), 0x20);
        assert_eq!(header.entry_count.get(), 7);
        assert_eq!(header.entry_list_ptr.get(), 0x40);
        assert_eq!(header.string_pool_size.get(), 0x20);
        assert_eq!(header.string_pool_ptr.get(), 0xE0);
        assert_eq!(header.next_free_id.get(), 2);
        assert!(!header.synced_ids.get());

        // The root is `ROOT` whatever its name; other nodes uppercase theirs.
        let root: &Node = r.view_at(NODES).unwrap();
        assert_eq!(&root.tag, b"ROOT");
        assert_eq!(root.name.get(), 5);
        assert_eq!(root.name_hash.get(), name_hash(b"root"));
        assert_eq!(root.entry_count.get(), 4);
        assert_eq!(root.first_entry.get(), 0);
        let sub: &Node = r.view_at(NODES + node::LEN).unwrap();
        assert_eq!(&sub.tag, b"SUB ");
        assert_eq!(sub.name.get(), 16);
        assert_eq!(sub.entry_count.get(), 3);
        assert_eq!(sub.first_entry.get(), 4);

        // Root's entries: `a.bin`, `sub`, then `.` and `..` last, the order
        // every retail directory uses. Directories share the id that is no
        // id, and the root's `..` points at nothing. Ids are handed out
        // lowest first as files are met, not by entry index: a.bin claims 0,
        // and b.bin claims 1 rather than the 4 its entry sits at.
        let entry = |index: usize| {
            let record: &Entry = r.view_at(ENTRIES + index * entry::LEN).unwrap();
            (
                record.id.get(),
                record.flags_and_name.get(),
                record.data_or_node.get(),
                record.data_size.get(),
            )
        };
        assert_eq!(entry(0), (0, 0x11 << 24 | 0x0A, 0, 5));
        assert_eq!(entry(1), (0xFFFF, 0x02 << 24 | 16, 1, 0x10));
        assert_eq!(entry(2), (0xFFFF, 0x02 << 24, 0, 0x10));
        assert_eq!(entry(3), (0xFFFF, 0x02 << 24 | 2, u32::MAX, 0x10));
        assert_eq!(entry(4), (1, 0x11 << 24 | 0x14, 0x20, 3));
        assert_eq!(entry(5), (0xFFFF, 0x02 << 24, 1, 0x10));
        assert_eq!(entry(6), (0xFFFF, 0x02 << 24 | 2, 0, 0x10));
        let a: &Entry = r.view_at(ENTRIES).unwrap();
        assert_eq!(a.name_hash.get(), name_hash(b"a.bin"));

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
        let r = Reader::new(&data);
        let header: &DataHeader = r.view_at(data_header::AT).unwrap();

        // Five directories, and the runs of entries they own: each node's
        // children, then its own `.` and `..`, laid out in node order.
        assert_eq!(header.node_count.get(), 5);
        assert_eq!(header.entry_count.get(), 17);

        let nodes = data_header::AT + data_header::LEN;
        let node = |index: usize| {
            let record: &Node = r.view_at(nodes + index * node::LEN).unwrap();
            (
                &record.tag,
                record.entry_count.get(),
                record.first_entry.get(),
            )
        };
        assert_eq!(node(0), (b"ROOT", 4, 0));
        assert_eq!(node(1), (b"A   ", 4, 4));
        assert_eq!(node(2), (b"SUB ", 3, 8));
        assert_eq!(node(3), (b"B   ", 3, 11));
        assert_eq!(node(4), (b"SUB ", 3, 14));

        // A nested directory's `..` names its own parent rather than the root:
        // it is the last entry of its run, and both `sub` nodes have one.
        let entries = data_header::AT + header.entry_list_ptr.get() as usize;
        let parent_of = |index: usize| {
            let record: &Entry = r.view_at(entries + index * entry::LEN).unwrap();
            record.data_or_node.get()
        };
        assert_eq!(parent_of(10), 1);
        assert_eq!(parent_of(16), 3);

        // Which is the tree the paths come back as, `z.bin` under `b/sub`
        // rather than under the `sub` that already existed.
        let opened = unpack(&data).unwrap();
        let paths: Vec<_> = opened.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["a/x.bin", "a/sub/y.bin", "b/sub/z.bin"]);
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// Both ends derive the next-free counter the same way, which is what
    /// leaves nothing for [`Archive::next_free_id`] to carry.
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
        let header: &DataHeader = Reader::new(&data).view_at(data_header::AT).unwrap();

        assert!(header.synced_ids.get());
        // The two files, then the root's `.` and `..`.
        assert_eq!(header.entry_count.get(), 4);
        assert_eq!(header.next_free_id.get(), 4);

        let opened = unpack(&data).unwrap();
        assert_eq!(opened.files[0].id, Some(0));
        assert_eq!(opened.files[1].id, Some(1));
        assert!(opened.next_free_id.is_none());
        assert_eq!(pack(&opened).unwrap(), data);
    }

    /// The next-free counter follows the carried id (9), not the entry count.
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
        let header: &DataHeader = Reader::new(&data).view_at(data_header::AT).unwrap();
        assert!(!header.synced_ids.get());
        assert_eq!(header.next_free_id.get(), 10);
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

    /// Compression is inferred from the bytes, not from anything on [`File`].
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
        assert_eq!(data[ENTRIES + offset_of!(Entry, flags_and_name)], 0x95);
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
        let top: &TopHeader = Reader::new(&data).view_at(0).unwrap();
        assert_eq!(top.mram_size.get(), 0x20);
        assert_eq!(top.aram_size.get(), 0x20);
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
        let top: &TopHeader = Reader::new(&data).view_at(0).unwrap();
        assert_eq!(top.mram_size.get(), 0x20);
        assert_eq!(top.aram_size.get(), 0x20);
    }

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
