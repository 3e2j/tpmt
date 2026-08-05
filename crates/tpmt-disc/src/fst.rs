//! The file table: everything on the disc that is not the preamble.
//!
//! A flat array in depth-first order. A directory holds the index its own
//! subtree ends at, so nesting is a matter of index ranges rather than pointers.
//! Names live in a pool after the array.
//!
//! Nothing about the table is kept when a disc is unpacked, because nothing in
//! it has to be. It is the directory tree written down, and the offsets it
//! carries are the one part a build works out for itself.

// TODO: writing, which is the whole reason the table is not unpacked. Entries
// go depth first, each directory's children ordered by their uppercased name,
// and the name pool follows in that same order, back to back and NUL
// terminated, with the root's own offset left at 0. Rebuilding the three prints
// that way reproduces their `fst.bin` byte for byte, length included. Ordering
// on the raw bytes instead misplaces 7 directories and a case-insensitive
// compare misplaces 4, and no two siblings share an uppercased name, so the
// order is total. The flag byte for a directory is 1, not the whole 0xFF the
// mask here would also accept.

use tpmt_bytes::Reader;

use crate::{Entry, Error, Result};

// A flags-and-name word, then two fields whose meaning depends on whether the
// entry is a directory.
pub(crate) const ENTRY_LEN: usize = 0x0C;
pub(crate) const DIRECTORY_FLAG: u32 = 0xFF00_0000;
pub(crate) const NAME_MASK: u32 = 0x00FF_FFFF;

/// Walks a file table, giving back what it holds in the order it holds it, each
/// at the path it will be unpacked to.
pub(crate) fn walk(fst: &[u8]) -> Result<Vec<Entry>> {
    let fst = Reader::new(fst);

    // The root entry is a directory covering everything, so its end index is the
    // number of entries in the table.
    let root = record(&fst, 0)?;
    if !root.is_directory {
        return Err(Error::CorruptFileTable("the root is not a directory"));
    }
    let total = root.end_or_size as usize;
    let names = total
        .checked_mul(ENTRY_LEN)
        .ok_or(Error::CorruptFileTable("the entry count is nonsense"))?;

    let mut entries = Vec::new();
    // Each frame is a directory still being walked: where its subtree ends and
    // what to prefix its members with.
    let mut open: Vec<(usize, String)> = vec![(total, "files".to_string())];

    for index in 1..total {
        while open.len() > 1 && index >= open[open.len() - 1].0 {
            open.pop();
        }

        let record = record(&fst, index)?;
        let name = fst.cstr_at(names + record.name_offset as usize)?;
        let name = name_of(name)?;

        let path = format!("{}/{name}", open[open.len() - 1].1);
        if record.is_directory {
            let end = record.end_or_size as usize;
            if end <= index || end > total {
                return Err(Error::CorruptFileTable("a directory ends before it starts"));
            }
            open.push((end, path.clone()));
            entries.push(Entry::Directory { path });
        } else {
            entries.push(Entry::File {
                path,
                offset: record.offset_or_parent as u64,
                size: record.end_or_size as u64,
            });
        }
    }

    Ok(entries)
}

/// Reads one name out of the pool.
///
/// Assumed Shift-JIS, following the archives on this disc, which demonstrably
/// are: a member name in `dmapres.arc` holds a fullwidth `ｘ` stored as `82 98`.
/// No name in the file table of any of the three prints has a byte over 0x7F,
/// so nothing here confirms it either way.
///
/// A name has to be one path component and nothing else, or it puts a file
/// somewhere the disc never asked for. Checked after decoding, since `0x5C` is
/// both a separator and the second byte of characters like `ソ`.
fn name_of(raw: &[u8]) -> Result<String> {
    let (name, _, malformed) = encoding_rs::SHIFT_JIS.decode(raw);
    if malformed {
        return Err(Error::CorruptFileTable("an entry name is not Shift-JIS"));
    }

    if name.is_empty()
        || name.contains(['/', '\\'])
        || name.chars().all(|c| c == '.')
        || name.chars().any(char::is_control)
    {
        return Err(Error::CorruptFileTable("an entry name is not a file name"));
    }
    Ok(name.into_owned())
}

/// One file table record. Its last two fields mean different things either side
/// of the directory flag: a file's data offset and length, or a directory's
/// parent index and the index its subtree ends at.
struct Record {
    is_directory: bool,
    name_offset: u32,
    offset_or_parent: u32,
    end_or_size: u32,
}

fn record(fst: &Reader, index: usize) -> Result<Record> {
    let at = index * ENTRY_LEN;
    let flags_and_name = fst.u32_at(at)?;
    Ok(Record {
        is_directory: flags_and_name & DIRECTORY_FLAG != 0,
        name_offset: flags_and_name & NAME_MASK,
        offset_or_parent: fst.u32_at(at + 4)?,
        end_or_size: fst.u32_at(at + 8)?,
    })
}
