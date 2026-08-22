//! The read path: turns an archive's bytes back into an [`Archive`], nothing
//! copied out of the input.

use tpmt_bytes::Reader;

use crate::{
    Archive, Error, File, Preload, Result, data_header, entry, name_hash, next_free_id, node,
    top_header,
};

/// One archive opened for reading: its bytes, and where each section starts.
///
/// The fixed [`top_header`] points at the [`data_header`], which in turn
/// points at the fields below in the order they're declared. The file
/// states those offsets relative to the data header; they're resolved to
/// absolute positions once, here, so nothing below has to carry the anchor
/// around.
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
    string_pool_at: usize,
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
        string_pool_at: relative(data_header::STRING_POOL_PTR)?,
        file_data_at: header.saturating_add(reader.u32_at(top_header::FILE_DATA_PTR)? as usize),
        reader,
    };
    // The root is node 0, and its name is the one thing read outside the walk.
    let name_at = opened.reader.u32_at(nodes_at + node::NAME)?;
    let hash = opened.reader.u16_at(nodes_at + node::NAME_HASH)?;
    let root = opened.name(name_at, hash)?;

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
        let raw = self.reader.cstr_at(self.string_pool_at + offset as usize)?;
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
