//! The write path: turns an [`Archive`] back into bytes.

use std::collections::HashSet;

use tpmt_bytes::Writer;

use crate::{
    Archive, Error, Preload, Result, data_header, entry, name_hash, next_free_id, node, top_header,
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
    fn build(archive: &Archive) -> Result<DirTree> {
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
            first_entry.push(entry_count as u32);
            entry_count += dirs[dir].children.len() + 2;
        }

        Ok(DirTree {
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
            at = match dirs[at].children.iter().find_map(|child| match child {
                Entry::Dir(dir) if dirs[*dir].name == name => Some(*dir),
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
                    dirs[at].children.push(Entry::Dir(dir));
                    dir
                }
            };
        }
    }

    Ok(dirs)
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
/// no other file in the list already claims, not the format's own convention
/// (see the comment above [`place_files`] for why). This only guards against
/// a collision within the list handed in; it cannot know whether some other
/// file, elsewhere, still references an id that a deleted file used to hold.
/// That is a linker's job once one exists.
///
/// An [`Archive::next_free_id`] the input didn't carry is derived here too
/// (despite never being used by our implementation).
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
    write_headers(&mut out, &tree, &sections, next_free, placed.synced);
    write_nodes(&mut out, &tree, &string_pool);
    write_entries(&mut out, archive, &tree, &string_pool, &placed)?;
    out.bytes(&string_pool.bytes);
    out.zeros(sections.string_pool_size - string_pool.bytes.len());
    write_file_data(&mut out, archive, &tree);

    // The four fields that need the finished file's length.
    let size = u32::try_from(out.len()).map_err(|_| Error::Oversized)?;
    out.u32_at(top_header::FILE_SIZE, size);
    out.u32_at(top_header::TOTAL_DATA_SIZE, size - sections.data_at as u32);
    out.u32_at(top_header::MRAM_SIZE, placed.mram as u32);
    out.u32_at(top_header::ARAM_SIZE, placed.aram as u32);
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
/// original (see [`pack`]'s doc, and the comment below for the format's own
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
    fn of(tree: &DirTree, string_pool_len: usize) -> SectionOffsets {
        let nodes_at = data_header::AT + data_header::LEN;
        let entries_at = (nodes_at + tree.order.len() * node::LEN).next_multiple_of(ALIGN);
        let string_pool_at = (entries_at + tree.entry_count * entry::LEN).next_multiple_of(ALIGN);
        let string_pool_size = string_pool_len.next_multiple_of(ALIGN);
        SectionOffsets {
            nodes_at,
            entries_at,
            string_pool_at,
            string_pool_size,
            data_at: string_pool_at + string_pool_size,
        }
    }
}

/// Both headers, which go out as zeros and are then patched field by field.
/// Whatever is never patched stays zero, which is what the unnamed fields hold
/// on a retail archive anyway.
fn write_headers(
    out: &mut Writer,
    tree: &DirTree,
    sections: &SectionOffsets,
    next_free: u16,
    synced: bool,
) {
    let header = data_header::AT;
    out.bytes(top_header::MAGIC);
    out.zeros(sections.nodes_at - top_header::MAGIC.len());
    out.u32_at(top_header::DATA_HEADER_PTR, header as u32);
    out.u32_at(
        top_header::FILE_DATA_PTR,
        (sections.data_at - header) as u32,
    );

    out.u32_at(header + data_header::NODE_COUNT, tree.order.len() as u32);
    out.u32_at(
        header + data_header::NODE_LIST_PTR,
        (sections.nodes_at - header) as u32,
    );
    out.u32_at(header + data_header::ENTRY_COUNT, tree.entry_count as u32);
    out.u32_at(
        header + data_header::ENTRY_LIST_PTR,
        (sections.entries_at - header) as u32,
    );
    out.u32_at(
        header + data_header::STRING_POOL_SIZE,
        sections.string_pool_size as u32,
    );
    out.u32_at(
        header + data_header::STRING_POOL_PTR,
        (sections.string_pool_at - header) as u32,
    );
    out.u16_at(header + data_header::NEXT_FREE_ID, next_free);
    out.u8_at(header + data_header::SYNCED_IDS, synced as u8);
}

/// One record per directory, in node order, naming the run of entries it holds.
fn write_nodes(out: &mut Writer, tree: &DirTree, string_pool: &StringPool) {
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
        out.u32(string_pool.dir_name_ats[dir]);
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
        dir_entry(out, b".", DOT_IN_STRING_POOL, node as u32);
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
    out.u16(entry::NO_ID);
    out.u16(name_hash(name));
    out.u32(entry::FLAG_DIRECTORY << entry::FLAGS_SHIFT | name_at);
    out.u32(node);
    out.u32(entry::DIRECTORY_SIZE);
    out.u32(0);
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
        | match data.starts_with(b"Yaz0") {
            true => entry::FLAG_COMPRESSED | entry::FLAG_YAZ0,
            false => 0,
        };

    out.u16(entry.id);
    out.u16(entry.hash);
    out.u32(flags << entry::FLAGS_SHIFT | entry.name_at);
    out.u32(entry.offset);
    out.u32(size);
    out.u32(0);
    Ok(())
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
