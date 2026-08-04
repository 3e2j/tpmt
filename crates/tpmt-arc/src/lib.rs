//! RARC archive containers.
//!
//! Distributed as `.arc`, these are containers of game assets. Nearly every
//! archive on the disc arrives Yaz0-compressed, but that wrapper comes off
//! before anything here sees it. Container only. What an entry holds is
//! somebody else's problem.
//!
//! The shape of one, every section starting 0x20 aligned:
//!
//! ```text
//! 0x00  top header    magic, sizes, where the data header and file data sit
//! 0x20  data header   node and entry counts and offsets, string pool, ids
//! 0x40  nodes         0x10 each: one per directory, naming a run of entries
//! ....  entries       0x14 each: one per member, `.` and `..` included
//! ....  string pool   the names, null terminated, Shift-JIS
//! ....  file data     member bytes, each padded out to 0x20
//! ```
//!
//! Everything below the top header is located by offsets relative to the data
//! header, including the file data offset, which sits in the top header
//! itself.

use std::collections::HashMap;
use tpmt_bytes::Reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a RARC archive")]
    NotRarc,

    // Never seen off a retail disc: every rule this enforces held on all three
    // prints. Anything tripping it is damaged or hand-made.
    #[error("the archive is corrupt: {0}")]
    Corrupt(&'static str),

    // A name is about to become a path component, so anything that could climb
    // out of the directory it is being written into is refused by name.
    #[error("`{0}` is not usable as a file name")]
    UnusableName(String),

    // `build` swaps member data in by path, so the set handed to it has to
    // match the archive's own list exactly. Anything else is refused rather
    // than guessed around.
    #[error("the archive has no member `{0}`")]
    UnknownMember(String),
    #[error("no data was given for `{0}`")]
    MissingMember(String),
    #[error("`{0}` was given twice")]
    DuplicateMember(String),

    #[error("the rebuilt archive would not fit its 32 bit size fields")]
    Oversized,

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One file in an archive, with its path relative to the archive root.
pub struct File<'a> {
    pub path: String,
    pub data: &'a [u8],
}

const MAGIC: &[u8; 4] = b"RARC";

// Top header.
const FILE_SIZE: usize = 0x04;
const DATA_HEADER_OFFSET: usize = 0x08;
// A top header field, but relative to the data header like everything below.
// References disagree on whether the anchor is the data header or 0x20; every
// retail archive pins the data header at 0x20, where nothing distinguishes
// the two.
const FILE_DATA_OFFSET: usize = 0x0C;
const TOTAL_DATA_SIZE: usize = 0x10;
// How much of the file data the game preloads into main against audio RAM,
// as marked per member by the preload type bits.
const MRAM_SIZE: usize = 0x14;
const ARAM_SIZE: usize = 0x18;

// Data header.
const NODE_COUNT: usize = 0x00;
const NODE_LIST_OFFSET: usize = 0x04;
const ENTRY_COUNT: usize = 0x08;
const ENTRY_LIST_OFFSET: usize = 0x0C;
const STRING_POOL_OFFSET: usize = 0x14;

// Node record.
const NODE_SIZE: usize = 0x10;
const NODE_FILE_COUNT: usize = 0x0A;
const NODE_FIRST_ENTRY: usize = 0x0C;

// File entry record.
const ENTRY_SIZE: usize = 0x14;
const ENTRY_NAME_HASH: usize = 0x02;
const ENTRY_TYPE_AND_NAME: usize = 0x04;
const ENTRY_DATA_OR_NODE: usize = 0x08;
const ENTRY_DATA_SIZE: usize = 0x0C;
const ENTRY_TYPE_SHIFT: u32 = 24;
const ENTRY_NAME_MASK: u32 = 0x00FF_FFFF;
const ENTRY_TYPE_DIRECTORY: u32 = 0x02;
const ENTRY_TYPE_COMPRESSED: u32 = 0x04;
const ENTRY_TYPE_MRAM: u32 = 0x10;
const ENTRY_TYPE_ARAM: u32 = 0x20;
const ENTRY_TYPE_YAZ0: u32 = 0x80;

/// Lists every file in an archive, directories flattened into the paths.
///
/// The data is borrowed, so a member's bytes cost nothing until somebody keeps
/// them.
pub fn files(data: &[u8]) -> Result<Vec<File<'_>>> {
    let members = Archive::open(data)?.members()?;
    Ok(members.into_iter().map(|(_, file)| file).collect())
}

// TODO: adding and removing members, once mods need it. That ends the prefix
// copy below: the node records, entry table and string pool would have to be
// re-emitted from the parsed tree. The conventions re-emission needs held on
// every archive of all three retail prints:
//
// - The pool is packed in reference order, `.` then `..` then the root name
//   first, repeated names stored again rather than shared, the whole thing
//   zero padded to 0x20. Node fourcc is the directory name ASCII-uppercased,
//   truncated to four, space padded, `ROOT` for the root. Hashes are
//   `name_hash` on every node and entry. Sections stay in order, each
//   starting 0x20 aligned.
// - Surviving members keep their copied ids and preload flags. A new member
//   takes the next free id, and the sync flag at data header 0x1A clears
//   when ids stop matching entry indices; retail ships 106 unsynced archives
//   per print, so the game handles that path. Preload defaults to MRAM
//   (0x10); ARAM (0x20) is only ever RELS.arc members.
// - Node indices are assigned depth-first, children in entry order. Probed
//   across all three prints: only three archives per print are deep enough
//   to tell depth from breadth first, and every one reads depth-first, with
//   the rest consistent with both.
//
// The regression harness already exists: handed an unchanged member set, a
// re-emitter must still rebuild every retail archive byte for byte, which
// also catches any misreading of the conventions above.

/// Builds the archive back up, each member's bytes swapped for its copy in
/// `members`. Swapping every member back in unchanged rebuilds the original
/// byte for byte.
///
/// This is a rebuild, not creation from scratch: everything above the file
/// data keeps its original bytes, which carries the authored state nothing
/// could derive (per-file ids, the id bookkeeping in the data header, the
/// preload flags) along untouched. Only the fields that describe member bytes
/// are rewritten: offsets, sizes, the compression bits, and the header
/// totals.
///
/// `members` must hold exactly the paths that [`files`] lists for the
/// original archive.
pub fn build(original: &[u8], members: &[File]) -> Result<Vec<u8>> {
    let archive = Archive::open(original)?;

    let mut fresh = HashMap::with_capacity(members.len());
    for member in members {
        if fresh.insert(member.path.as_str(), member.data).is_some() {
            return Err(Error::DuplicateMember(member.path.clone()));
        }
    }
    // Replacement data keyed by entry index, since the entry table is about
    // to be walked flat rather than as a tree.
    let mut swaps = HashMap::with_capacity(fresh.len());
    for (index, file) in archive.members()? {
        let Some(data) = fresh.remove(file.path.as_str()) else {
            return Err(Error::MissingMember(file.path));
        };
        swaps.insert(index, data);
    }
    if let Some(path) = fresh.into_keys().next() {
        return Err(Error::UnknownMember(path.to_owned()));
    }

    let mut out = archive.reader.slice_at(0, archive.file_data)?.to_vec();
    let mut mram = 0;
    let mut aram = 0;

    for index in 0..archive.entry_count {
        let record = archive.entries + index * ENTRY_SIZE;
        let type_and_name = archive.reader.u32_at(record + ENTRY_TYPE_AND_NAME)?;
        let flags = type_and_name >> ENTRY_TYPE_SHIFT;
        if flags & ENTRY_TYPE_DIRECTORY != 0 {
            continue;
        }
        let Some(&data) = swaps.get(&index) else {
            return Err(Error::Corrupt("a file entry is outside every directory"));
        };

        // The compression bits restate what the member's bytes already are.
        let compressed = ENTRY_TYPE_COMPRESSED | ENTRY_TYPE_YAZ0;
        let flags = match data.starts_with(b"Yaz0") {
            true => flags | compressed,
            false => flags & !compressed,
        };

        let offset = out.len() - archive.file_data;
        put32(&mut out, record + ENTRY_TYPE_AND_NAME, {
            flags << ENTRY_TYPE_SHIFT | type_and_name & ENTRY_NAME_MASK
        });
        put32(&mut out, record + ENTRY_DATA_OR_NODE, offset as u32);
        put32(&mut out, record + ENTRY_DATA_SIZE, data.len() as u32);

        let padded = data.len().next_multiple_of(0x20);
        if flags & ENTRY_TYPE_MRAM != 0 {
            mram += padded;
        }
        if flags & ENTRY_TYPE_ARAM != 0 {
            aram += padded;
        }
        out.extend_from_slice(data);
        out.resize(archive.file_data + offset + padded, 0);
    }

    let size = u32::try_from(out.len()).map_err(|_| Error::Oversized)?;
    put32(&mut out, FILE_SIZE, size);
    put32(&mut out, TOTAL_DATA_SIZE, size - archive.file_data as u32);
    put32(&mut out, MRAM_SIZE, mram as u32);
    put32(&mut out, ARAM_SIZE, aram as u32);
    Ok(out)
}

fn put32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

struct Archive<'a> {
    reader: Reader<'a>,
    node_count: usize,
    nodes: usize,
    entry_count: usize,
    entries: usize,
    strings: usize,
    file_data: usize,
}

impl<'a> Archive<'a> {
    fn open(data: &'a [u8]) -> Result<Self> {
        if !data.starts_with(MAGIC) {
            return Err(Error::NotRarc);
        }

        let reader = Reader::new(data);
        if reader.u32_at(FILE_SIZE)? as usize != data.len() {
            return Err(Error::Corrupt("the stated size is not the actual size"));
        }

        let header = reader.u32_at(DATA_HEADER_OFFSET)? as usize;
        let relative = |field| -> Result<usize> {
            Ok(header.saturating_add(reader.u32_at(header + field)? as usize))
        };

        let node_count = reader.u32_at(header + NODE_COUNT)? as usize;
        let nodes = relative(NODE_LIST_OFFSET)?;
        let entry_count = reader.u32_at(header + ENTRY_COUNT)? as usize;
        let entries = relative(ENTRY_LIST_OFFSET)?;

        // A bad count is refused before it can size an allocation or a walk,
        // so past here both tables are known to sit inside the buffer.
        let fits = |offset: usize, count: usize, record: usize| {
            count
                .checked_mul(record)
                .and_then(|len| offset.checked_add(len))
                .is_some_and(|end| end <= data.len())
        };
        if node_count == 0 {
            return Err(Error::Corrupt("there is no root directory"));
        }
        if !fits(nodes, node_count, NODE_SIZE) {
            return Err(Error::Corrupt(
                "more directories than the archive could hold",
            ));
        }
        if !fits(entries, entry_count, ENTRY_SIZE) {
            return Err(Error::Corrupt("more entries than the archive could hold"));
        }

        Ok(Self {
            node_count,
            nodes,
            entry_count,
            entries,
            strings: relative(STRING_POOL_OFFSET)?,
            file_data: header.saturating_add(reader.u32_at(FILE_DATA_OFFSET)? as usize),
            reader,
        })
    }

    /// Walks the directory tree from the root node, which is always node 0.
    /// Each file is paired with the index of its entry record, which is what
    /// `build` keys replacement data on.
    fn members(&self) -> Result<Vec<(usize, File<'a>)>> {
        let mut members = Vec::with_capacity(self.entry_count);
        let mut visited = vec![false; self.node_count];
        // Each frame is a directory mid-walk: the entries it still owes, and
        // the path prefix its members go under.
        let mut stack = vec![(self.open_node(0, &mut visited)?, String::new())];

        while let Some((range, prefix)) = stack.last_mut() {
            let Some(index) = range.next() else {
                stack.pop();
                continue;
            };

            let record = self.entries + index * ENTRY_SIZE;
            let type_and_name = self.reader.u32_at(record + ENTRY_TYPE_AND_NAME)?;
            let hash = self.reader.u16_at(record + ENTRY_NAME_HASH)?;
            let name = self.name(type_and_name & ENTRY_NAME_MASK, hash)?;

            // Every directory carries `.` and `..` entries pointing at itself
            // and its parent. They are structure, not content.
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
            let target = self.reader.u32_at(record + ENTRY_DATA_OR_NODE)? as usize;

            if type_and_name >> ENTRY_TYPE_SHIFT & ENTRY_TYPE_DIRECTORY != 0 {
                let range = self.open_node(target, &mut visited)?;
                stack.push((range, path));
            } else {
                let size = self.reader.u32_at(record + ENTRY_DATA_SIZE)? as usize;
                let file = File {
                    path,
                    data: self.reader.slice_at(self.file_data + target, size)?,
                };
                members.push((index, file));
            }
        }

        Ok(members)
    }

    /// Marks a node visited and hands back the run of entries it owns.
    fn open_node(&self, node: usize, visited: &mut [bool]) -> Result<std::ops::Range<usize>> {
        // A directory aimed at a missing node, or back at an ancestor, cannot
        // happen in a well-formed archive. Neither is stepped over quietly:
        // dropping a subtree here would look exactly like success.
        match visited.get_mut(node) {
            None => {
                return Err(Error::Corrupt(
                    "a directory points at a node that does not exist",
                ));
            }
            Some(true) => return Err(Error::Corrupt("the directory tree loops")),
            Some(seen) => *seen = true,
        }

        let record = self.nodes + node * NODE_SIZE;
        let first = self.reader.u32_at(record + NODE_FIRST_ENTRY)? as usize;
        let count = self.reader.u16_at(record + NODE_FILE_COUNT)? as usize;
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
        let raw = self.reader.cstr_at(self.strings + offset as usize)?;
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

    const HEADER: usize = 0x20;
    const NODES: usize = 0x40;
    const ENTRIES: usize = 0x60;
    const STRINGS: usize = 0xEC;
    const FILE_DATA: usize = 0x110;

    // String pool positions within `.\0..\0sub\0a.bin\0b.bin\0`.
    const NAME_SELF: u32 = 0;
    const NAME_PARENT: u32 = 2;
    const NAME_SUB: u32 = 5;
    const NAME_A: u32 = 9;
    const NAME_B: u32 = 15;

    const NO_NODE: u32 = 0xFFFF_FFFF;
    const DIRECTORY: u32 = 0x02;
    const REGULAR: u32 = 0x11;

    fn put16(data: &mut [u8], at: usize, value: u16) {
        data[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// Writes an entry, hashing whatever name the pool holds at `name`, so the
    /// pool has to be in place first.
    fn put_entry(data: &mut [u8], index: usize, kind: u32, name: u32, target: u32, size: u32) {
        let start = STRINGS + name as usize;
        let len = data[start..].iter().position(|&b| b == 0).unwrap();
        let hash = name_hash(&data[start..start + len]);

        let at = ENTRIES + index * ENTRY_SIZE;
        put16(data, at + ENTRY_NAME_HASH, hash);
        put32(
            data,
            at + ENTRY_TYPE_AND_NAME,
            kind << ENTRY_TYPE_SHIFT | name,
        );
        put32(data, at + ENTRY_DATA_OR_NODE, target);
        put32(data, at + ENTRY_DATA_SIZE, size);
    }

    /// A root holding `a.bin` and a subdirectory `sub` holding `b.bin`.
    /// `sub_target` is the node the `sub` entry points at, so a caller can aim
    /// it somewhere it has no business pointing.
    fn archive(sub_target: u32) -> Vec<u8> {
        let mut data = vec![0u8; FILE_DATA];
        data[..MAGIC.len()].copy_from_slice(MAGIC);
        put32(&mut data, DATA_HEADER_OFFSET, HEADER as u32);
        put32(&mut data, FILE_DATA_OFFSET, (FILE_DATA - HEADER) as u32);

        put32(&mut data, HEADER + NODE_COUNT, 2);
        put32(
            &mut data,
            HEADER + NODE_LIST_OFFSET,
            (NODES - HEADER) as u32,
        );
        put32(&mut data, HEADER + ENTRY_COUNT, 7);
        put32(
            &mut data,
            HEADER + ENTRY_LIST_OFFSET,
            (ENTRIES - HEADER) as u32,
        );
        put32(
            &mut data,
            HEADER + STRING_POOL_OFFSET,
            (STRINGS - HEADER) as u32,
        );

        // Root owns entries 0..4, `sub` owns 4..7.
        put16(&mut data, NODES + NODE_FILE_COUNT, 4);
        put32(&mut data, NODES + NODE_FIRST_ENTRY, 0);
        put16(&mut data, NODES + NODE_SIZE + NODE_FILE_COUNT, 3);
        put32(&mut data, NODES + NODE_SIZE + NODE_FIRST_ENTRY, 4);

        let strings = b".\0..\0sub\0a.bin\0b.bin\0";
        data[STRINGS..STRINGS + strings.len()].copy_from_slice(strings);

        put_entry(&mut data, 0, DIRECTORY, NAME_SELF, 0, 0x10);
        put_entry(&mut data, 1, DIRECTORY, NAME_PARENT, NO_NODE, 0x10);
        put_entry(&mut data, 2, DIRECTORY, NAME_SUB, sub_target, 0x10);
        put_entry(&mut data, 3, REGULAR, NAME_A, 0, 5);
        put_entry(&mut data, 4, DIRECTORY, NAME_SELF, 1, 0x10);
        put_entry(&mut data, 5, DIRECTORY, NAME_PARENT, 0, 0x10);
        put_entry(&mut data, 6, REGULAR, NAME_B, 0x20, 3);

        // Members in entry order, each padded out to 0x20, the layout `build`
        // reproduces.
        data.resize(FILE_DATA + 0x40, 0);
        data[FILE_DATA..FILE_DATA + 5].copy_from_slice(b"AAAAA");
        data[FILE_DATA + 0x20..FILE_DATA + 0x23].copy_from_slice(b"BBB");
        put32(&mut data, TOTAL_DATA_SIZE, 0x40);
        put32(&mut data, MRAM_SIZE, 0x40);
        let size = data.len() as u32;
        put32(&mut data, FILE_SIZE, size);
        data
    }

    /// Rewrites the first byte of `b.bin`'s pool name, hash kept in step, so a
    /// test can feed decode particular name bytes.
    fn rename_b(data: &mut [u8], byte: u8) {
        data[STRINGS + NAME_B as usize] = byte;
        put_entry(data, 6, REGULAR, NAME_B, 0x20, 3);
    }

    #[test]
    fn flattens_directories_into_paths() {
        let data = archive(1);
        let files = files(&data).unwrap();
        let listed: Vec<_> = files
            .iter()
            .map(|file| (file.path.as_str(), file.data))
            .collect();

        // Entry order, so the subdirectory comes out before the file after it.
        assert_eq!(
            listed,
            [
                ("sub/b.bin", b"BBB".as_slice()),
                ("a.bin", b"AAAAA".as_slice()),
            ]
        );
    }

    #[test]
    fn decodes_shift_jis_names() {
        let mut data = archive(1);
        // Halfwidth katakana RI, one byte in Shift-JIS.
        rename_b(&mut data, 0xD8);
        let files = files(&data).unwrap();
        assert_eq!(files[0].path, "sub/ﾘ.bin");
    }

    /// The disc has no cycles, so one marks the archive corrupt rather than
    /// producing a partial listing.
    #[test]
    fn a_directory_cycle_is_refused() {
        let data = archive(0);
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_dangling_directory_is_refused() {
        let data = archive(9);
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(files(b"Yaz0...."), Err(Error::NotRarc)));
    }

    /// A truncated archive no longer matches its own size field, and is turned
    /// away before any table read walks off the end.
    #[test]
    fn rejects_a_truncated_archive() {
        let data = archive(1);
        assert!(matches!(
            files(&data[..data.len() - 4]),
            Err(Error::Corrupt(_))
        ));
    }

    /// A nonsense count is caught up front, where it has not yet sized an
    /// allocation.
    #[test]
    fn rejects_counts_that_cannot_fit() {
        let mut data = archive(1);
        put32(&mut data, HEADER + ENTRY_COUNT, u32::MAX);
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_directory_claiming_missing_entries() {
        let mut data = archive(1);
        put16(&mut data, NODES + NODE_FILE_COUNT, 100);
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_name_with_a_separator() {
        let mut data = archive(1);
        rename_b(&mut data, b'/');
        assert!(matches!(files(&data), Err(Error::UnusableName(_))));
    }

    #[test]
    fn rejects_a_name_that_is_not_shift_jis() {
        let mut data = archive(1);
        // A lead byte with no trail byte after it.
        rename_b(&mut data, 0x85);
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_wrong_name_hash() {
        let mut data = archive(1);
        put16(
            &mut data,
            ENTRIES + 6 * ENTRY_SIZE + ENTRY_NAME_HASH,
            0xBEEF,
        );
        assert!(matches!(files(&data), Err(Error::Corrupt(_))));
    }

    /// The fidelity contract: handing every member back unchanged reproduces
    /// the original bytes.
    #[test]
    fn rebuilds_the_original_byte_for_byte() {
        let data = archive(1);
        let rebuilt = build(&data, &files(&data).unwrap()).unwrap();
        assert_eq!(rebuilt, data);
    }

    /// Growing a member pushes everything after it, and the result still
    /// decodes to the swapped contents.
    #[test]
    fn rebuilds_around_replaced_data() {
        let data = archive(1);
        let mut members = files(&data).unwrap();
        // a.bin, which sits before b.bin in the data section.
        members[1].data = &[0xAB; 0x21];
        let rebuilt = build(&data, &members).unwrap();

        let listed = files(&rebuilt).unwrap();
        assert_eq!(listed[0].path, "sub/b.bin");
        assert_eq!(listed[0].data, b"BBB");
        assert_eq!(listed[1].path, "a.bin");
        assert_eq!(listed[1].data, [0xAB; 0x21].as_slice());
    }

    /// The compression bits follow the bytes, so swapping Yaz0 data into a
    /// plain member marks it compressed.
    #[test]
    fn compression_bits_follow_the_member_bytes() {
        let data = archive(1);
        let mut members = files(&data).unwrap();
        members[0].data = b"Yaz0 in shape only";
        let rebuilt = build(&data, &members).unwrap();

        let flags = rebuilt[ENTRIES + 6 * ENTRY_SIZE + ENTRY_TYPE_AND_NAME];
        assert_eq!(
            flags as u32,
            REGULAR | ENTRY_TYPE_COMPRESSED | ENTRY_TYPE_YAZ0
        );
    }

    #[test]
    fn refuses_a_missing_member() {
        let data = archive(1);
        let members = files(&data).unwrap();
        assert!(matches!(
            build(&data, &members[..1]),
            Err(Error::MissingMember(_))
        ));
    }

    #[test]
    fn refuses_a_member_the_archive_does_not_have() {
        let data = archive(1);
        let mut members = files(&data).unwrap();
        members.push(File {
            path: "c.bin".into(),
            data: b"",
        });
        assert!(matches!(
            build(&data, &members),
            Err(Error::UnknownMember(_))
        ));
    }

    #[test]
    fn refuses_a_member_given_twice() {
        let data = archive(1);
        let mut members = files(&data).unwrap();
        members.push(File {
            path: "a.bin".into(),
            data: b"rival copy",
        });
        assert!(matches!(
            build(&data, &members),
            Err(Error::DuplicateMember(_))
        ));
    }
}
