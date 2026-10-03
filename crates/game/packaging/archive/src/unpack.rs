//! The read path: turns an archive's bytes back into an [`Archive`], nothing
//! copied out of the input.

use std::borrow::Cow;

use tpmt_binary::{Be32, Reader};

use crate::{
    Archive, DataHeader, Entry, Error, File, Node, Preload, Result, TopHeader, name_hash,
    next_free_id,
};

/// One archive opened for reading: its bytes, and where each section starts.
///
/// The fixed [`TopHeader`] points at the [`DataHeader`], which in turn
/// points at the fields below in the order they're declared. The file
/// states those offsets relative to the data header; they're resolved to
/// absolute positions once, here, so nothing below has to carry the anchor
/// around.
struct ArchiveReader<'a> {
    /// The whole archive, every read out of it bounds checked.
    reader: Reader<'a>,
    /// One record per directory, naming the run of entries it holds. Never
    /// empty, since node 0 is the root.
    nodes: &'a [Node],
    /// One record per file and per directory, `.` and `..` included. A
    /// directory's record points at its node, a file's at its bytes.
    entries: &'a [Entry],
    /// Every name, null terminated and Shift-JIS, referred to by offset from
    /// the start of it.
    string_pool_at: usize,
    /// The files' bytes, each padded out to 0x20. Last section in the archive.
    file_data_at: usize,
}

pub fn unpack(data: &[u8]) -> Result<Archive<'_>> {
    let reader = Reader::new(data);
    let top: &TopHeader = reader.view_at(0)?;
    if top.file_size.get() as usize != data.len() {
        return Err(Error::Corrupt("the stated size is not the actual size"));
    }

    // Helper: resolves an offset field in the data header to a position in
    // the archive. A nonsense offset saturates rather than wrapping to something
    // small, so it stays out of bounds and is caught by the checks below or by
    // the first read that follows it.
    let header = top.data_header_ptr.get() as usize;
    let relative = |field: Be32| header.saturating_add(field.get() as usize);

    let data_header: &DataHeader = reader.view_at(header)?;

    // A bad count is refused before it can size an allocation or a walk.
    let nodes: &[Node] = reader
        .slice_of(
            relative(data_header.node_list_ptr),
            data_header.node_count.get() as usize,
        )
        .map_err(|_| Error::Corrupt("more directories than the archive could hold"))?;
    let Some(root_node) = nodes.first() else {
        return Err(Error::Corrupt("there is no root directory"));
    };
    let entries: &[Entry] = reader
        .slice_of(
            relative(data_header.entry_list_ptr),
            data_header.entry_count.get() as usize,
        )
        .map_err(|_| Error::Corrupt("more entries than the archive could hold"))?;

    let file_data_at = relative(top.file_data_ptr);
    // The three fields below are never read again once this passes: nothing
    // downstream needs a stated size, only the actual bytes.
    if top.total_data_size.get() as usize != data.len().saturating_sub(file_data_at) {
        return Err(Error::Corrupt(
            "the stated data size does not match the file",
        ));
    }
    let mram_size = top.mram_size.get() as usize;
    let aram_size = top.aram_size.get() as usize;
    if mram_size
        .checked_add(aram_size)
        .is_none_or(|preloaded| preloaded > data.len() - file_data_at)
    {
        return Err(Error::Corrupt(
            "the preload sizes are larger than the data section",
        ));
    }

    let opened = ArchiveReader {
        nodes,
        entries,
        string_pool_at: relative(data_header.string_pool_ptr),
        file_data_at,
        reader,
    };
    // The root is node 0, and its name is the one thing read outside the walk.
    let root = opened
        .name(root_node.name.get(), root_node.name_hash.get())?
        .into_owned();

    // Only used for the verification below, never again: this is the one
    // place anything reads the stored counter back.
    let stored = data_header.next_free_id.get();
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
        let mut files = Vec::with_capacity(self.entries.len());
        let mut visited = vec![false; self.nodes.len()];
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

            let record = &self.entries[index];
            let flags = record.flags;
            let name = self.name(record.name_offset(), record.name_hash.get())?;

            // Every directory carries a `.` entry pointing at itself and a
            // `..` pointing at its parent, the only link back up.
            // The walk descends with its own stack and needs neither.
            if name == "." || name == ".." {
                continue;
            }
            if name.is_empty() || name.contains(['/', '\\']) {
                return Err(Error::UnusableName(name.into_owned()));
            }

            let path = if prefix.is_empty() {
                name.into_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let target = record.data_or_node.get() as usize;

            if flags & Entry::FLAG_DIRECTORY != 0 {
                let range = self.open_node(target, &mut visited)?;
                stack.push((range, path));
            } else {
                // A file's target is the offset of its bytes within the data
                // section, and exactly one of the three memory bits is set.
                let size = record.data_size.get() as usize;
                let preload = if flags & Entry::FLAG_MRAM != 0 {
                    Preload::Mram
                } else if flags & Entry::FLAG_ARAM != 0 {
                    Preload::Aram
                } else if flags & Entry::FLAG_DISC != 0 {
                    Preload::Disc
                } else {
                    return Err(Error::Corrupt("a file is marked for no memory at all"));
                };
                let id = record.id.get();
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

        Ok((files, next_free_id(self.entries.len(), highest, synced)?))
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

        let record = &self.nodes[index];
        let first = record.first_entry.get() as usize;
        let count = record.entry_count.get() as usize;
        first
            .checked_add(count)
            .filter(|&end| end <= self.entries.len())
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
    fn name(&self, offset: u32, hash: u16) -> Result<Cow<'a, str>> {
        let raw = self.reader.cstr_at(self.string_pool_at + offset as usize)?;
        if name_hash(raw) != hash {
            return Err(Error::Corrupt("a name does not match its stored hash"));
        }

        encoding_rs::SHIFT_JIS
            .decode_without_bom_handling_and_without_replacement(raw)
            .ok_or(Error::Corrupt("a name is not Shift-JIS"))
    }
}

#[cfg(test)]
mod tests {
    use tpmt_binary::{Be16, Layout, view_at_mut};

    use super::*;
    use crate::Format;
    use crate::pack::{
        self,
        tests::{ENTRIES, NAME_A, NODES, STRINGS, archive},
    };

    fn top_header(data: &mut [u8]) -> &mut TopHeader {
        view_at_mut(data, 0).unwrap()
    }

    fn data_header(data: &mut [u8]) -> &mut DataHeader {
        view_at_mut(data, DataHeader::AT).unwrap()
    }

    /// The fixture's entry at `index`: `a.bin` at 0, `sub` at 1.
    fn entry(data: &mut [u8], index: usize) -> &mut Entry {
        view_at_mut(data, ENTRIES + index * Entry::LEN).unwrap()
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

        assert_eq!(pack::pack(&opened).unwrap(), data);
    }

    /// A counter that is not what the ids come to is the archive's own, and a
    /// few were stored that way. Nothing else can bring it back, so it is
    /// carried, and a counter that is what they come to is not.
    #[test]
    fn a_counter_of_its_own_is_carried() {
        let mut data = archive();
        assert!(unpack(&data).unwrap().next_free_id.is_none());

        let header = data_header(&mut data);
        let stored = header.next_free_id.get() + 3;
        header.next_free_id = Be16::new(stored);

        let opened = unpack(&data).unwrap();
        assert_eq!(opened.next_free_id, Some(stored));
        assert_eq!(pack::pack(&opened).unwrap(), data);
    }

    #[test]
    fn decodes_shift_jis_names() {
        let mut data = archive();
        // Halfwidth katakana RI, one byte in Shift-JIS.
        data[STRINGS + NAME_A] = 0xD8;
        entry(&mut data, 0).name_hash = Be16::new(name_hash(b"\xD8.bin"));
        let opened = unpack(&data).unwrap();
        assert_eq!(opened.files[0].path, "ﾘ.bin");
        // And the trip back spells it in Shift-JIS again.
        assert_eq!(pack::pack(&opened).unwrap(), data);
    }

    #[test]
    fn rejects_other_data() {
        assert!(matches!(
            Archive::decode(b"Yaz0...."),
            Err(Error::WrongKind(_))
        ));
    }

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
        data_header(&mut data).node_count = Be32::new(0);
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
        let counts: [(fn(&mut DataHeader), _); 2] = [
            (
                |header| header.node_count = Be32::new(u32::MAX),
                "more directories than the archive could hold",
            ),
            (
                |header| header.entry_count = Be32::new(u32::MAX),
                "more entries than the archive could hold",
            ),
        ];
        for (corrupt, complaint) in counts {
            let mut data = archive();
            corrupt(data_header(&mut data));
            assert!(
                matches!(unpack(&data), Err(Error::Corrupt(message)) if message == complaint),
                "{complaint}"
            );
        }
    }

    #[test]
    fn rejects_a_directory_claiming_missing_entries() {
        let mut data = archive();
        view_at_mut::<Node>(&mut data, NODES).unwrap().entry_count = Be16::new(100);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_directory_cycle_is_refused() {
        let mut data = archive();
        // Aim `sub`'s entry back at the root's node.
        entry(&mut data, 1).data_or_node = Be32::new(0);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_dangling_directory_is_refused() {
        let mut data = archive();
        entry(&mut data, 1).data_or_node = Be32::new(9);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_name_with_a_separator() {
        let mut data = archive();
        data[STRINGS + NAME_A + 1] = b'/';
        entry(&mut data, 0).name_hash = Be16::new(name_hash(b"a/bin"));
        assert!(matches!(unpack(&data), Err(Error::UnusableName(_))));
    }

    #[test]
    fn rejects_a_name_that_is_not_shift_jis() {
        let mut data = archive();
        // A lead byte with no trail byte after it.
        data[STRINGS + NAME_A] = 0x85;
        entry(&mut data, 0).name_hash = Be16::new(name_hash(b"\x85.bin"));
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    /// A name opening with a UTF-16 byte order mark is still read as
    /// Shift-JIS, where `FF` is never valid, rather than as the UTF-16 the
    /// mark claims, which would pack back as different bytes.
    #[test]
    fn a_byte_order_mark_does_not_switch_encoding() {
        let mut data = archive();
        let name = b"\xFF\xFEAA";
        data[STRINGS + NAME_A..][..name.len()].copy_from_slice(name);
        data[STRINGS + NAME_A + name.len()] = 0;
        entry(&mut data, 0).name_hash = Be16::new(name_hash(name));
        assert!(matches!(
            unpack(&data),
            Err(Error::Corrupt("a name is not Shift-JIS"))
        ));
    }

    #[test]
    fn rejects_a_wrong_name_hash() {
        let mut data = archive();
        entry(&mut data, 0).name_hash = Be16::new(0xBEEF);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_file_marked_for_no_memory() {
        let mut data = archive();
        entry(&mut data, 0).flags = Entry::FLAG_FILE;
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_wrong_total_data_size() {
        let mut data = archive();
        top_header(&mut data).total_data_size = Be32::new(0);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_preload_sizes_bigger_than_the_data_section() {
        let mut data = archive();
        top_header(&mut data).mram_size = Be32::new(u32::MAX);
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }
}
