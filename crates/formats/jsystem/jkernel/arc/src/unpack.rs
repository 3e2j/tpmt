//! The read path: turns an archive's bytes back into an [`Archive`], nothing
//! copied out of the input.

use tpmt_bytes::{Be32, Reader};

use crate::{
    Archive, Error, File, Preload, Result,
    data_header::DataHeader,
    entry::{self, Entry},
    name_hash, next_free_id,
    node::Node,
    top_header::TopHeader,
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
    let root = opened.name(root_node.name.get(), root_node.name_hash.get())?;

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
            let flags_and_name = record.flags_and_name.get();
            let flags = flags_and_name >> entry::FLAGS_SHIFT;
            let name = self.name(flags_and_name & entry::NAME_MASK, record.name_hash.get())?;

            // Every directory carries a `.` entry pointing at itself and a
            // `..` pointing at its parent, the only link back up.
            // The walk descends with its own stack and needs neither.
            if name == "." || name == ".." {
                continue;
            }
            if name.is_empty() || name.contains(['/', '\\']) {
                return Err(Error::UnusableName(name));
            }

            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let target = record.data_or_node.get() as usize;

            if flags & entry::FLAG_DIRECTORY != 0 {
                let range = self.open_node(target, &mut visited)?;
                stack.push((range, path));
            } else {
                // A file's target is the offset of its bytes within the data
                // section, and exactly one of the three memory bits is set.
                let size = record.data_size.get() as usize;
                let preload = if flags & entry::FLAG_MRAM != 0 {
                    Preload::Mram
                } else if flags & entry::FLAG_ARAM != 0 {
                    Preload::Aram
                } else if flags & entry::FLAG_DISC != 0 {
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
    fn name(&self, offset: u32, hash: u16) -> Result<String> {
        let raw = self.reader.cstr_at(self.string_pool_at + offset as usize)?;
        if name_hash(raw) != hash {
            return Err(Error::Corrupt("a name does not match its stored hash"));
        }

        let (name, _, malformed) = encoding_rs::SHIFT_JIS.decode(raw);
        if malformed {
            Err(Error::Corrupt("a name is not Shift-JIS"))
        } else {
            Ok(name.into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use tpmt_bytes::Writer;

    use super::*;
    use crate::Format;
    use crate::data_header;
    use crate::pack::{
        self,
        tests::{ENTRIES, NAME_A, NODES, STRINGS, archive},
    };

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

        let at = data_header::AT + offset_of!(DataHeader, next_free_id);
        let stored = Reader::new(&data).u16_at(at).unwrap() + 3;
        data[at..at + 2].copy_from_slice(&stored.to_be_bytes());

        let opened = unpack(&data).unwrap();
        assert_eq!(opened.next_free_id, Some(stored));
        assert_eq!(pack::pack(&opened).unwrap(), data);
    }

    #[test]
    fn decodes_shift_jis_names() {
        let mut w = Writer::from(archive());
        // Halfwidth katakana RI, one byte in Shift-JIS.
        w.u8_at(STRINGS + NAME_A, 0xD8);
        w.u16_at(
            ENTRIES + offset_of!(Entry, name_hash),
            name_hash(b"\xD8.bin"),
        );
        let data = w.finish();
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
        let mut w = Writer::from(archive());
        w.u32_at(data_header::AT + offset_of!(DataHeader, node_count), 0);
        let data = w.finish();
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
                offset_of!(DataHeader, node_count),
                "more directories than the archive could hold",
            ),
            (
                offset_of!(DataHeader, entry_count),
                "more entries than the archive could hold",
            ),
        ];
        for (field, complaint) in counts {
            let mut w = Writer::from(archive());
            w.u32_at(data_header::AT + field, u32::MAX);
            let data = w.finish();
            assert!(
                matches!(unpack(&data), Err(Error::Corrupt(message)) if message == complaint),
                "{complaint}"
            );
        }
    }

    #[test]
    fn rejects_a_directory_claiming_missing_entries() {
        let mut w = Writer::from(archive());
        w.u16_at(NODES + offset_of!(Node, entry_count), 100);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_directory_cycle_is_refused() {
        let mut w = Writer::from(archive());
        // Aim `sub`'s entry back at the root's node.
        w.u32_at(ENTRIES + entry::LEN + offset_of!(Entry, data_or_node), 0);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn a_dangling_directory_is_refused() {
        let mut w = Writer::from(archive());
        w.u32_at(ENTRIES + entry::LEN + offset_of!(Entry, data_or_node), 9);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_name_with_a_separator() {
        let mut w = Writer::from(archive());
        w.u8_at(STRINGS + NAME_A + 1, b'/');
        w.u16_at(ENTRIES + offset_of!(Entry, name_hash), name_hash(b"a/bin"));
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::UnusableName(_))));
    }

    #[test]
    fn rejects_a_name_that_is_not_shift_jis() {
        let mut w = Writer::from(archive());
        // A lead byte with no trail byte after it.
        w.u8_at(STRINGS + NAME_A, 0x85);
        w.u16_at(
            ENTRIES + offset_of!(Entry, name_hash),
            name_hash(b"\x85.bin"),
        );
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_wrong_name_hash() {
        let mut w = Writer::from(archive());
        w.u16_at(ENTRIES + offset_of!(Entry, name_hash), 0xBEEF);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_file_marked_for_no_memory() {
        let mut data = archive();
        data[ENTRIES + offset_of!(Entry, flags_and_name)] = 0x01;
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_a_wrong_total_data_size() {
        let mut w = Writer::from(archive());
        w.u32_at(offset_of!(TopHeader, total_data_size), 0);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn rejects_preload_sizes_bigger_than_the_data_section() {
        let mut w = Writer::from(archive());
        w.u32_at(offset_of!(TopHeader, mram_size), u32::MAX);
        let data = w.finish();
        assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
    }
}
