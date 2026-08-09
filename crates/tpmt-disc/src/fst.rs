//! The file table: everything on the disc that is not the preamble.
//!
//! A flat array in depth-first order. A directory holds the index its own
//! subtree ends at, so nesting is a matter of index ranges rather than pointers.
//! Names live in a pool after the array.
//!
//! Nothing about the table is kept when a disc is unpacked, because nothing in
//! it has to be. It is the directory tree written down, and the offsets it
//! carries are the one part a build works out for itself.

use std::collections::{HashMap, HashSet};

use tpmt_bytes::{Reader, Writer};

use crate::{Entry, Error, Item, Result};

// A flags-and-name word, then two fields whose meaning depends on whether the
// entry is a directory.
pub(crate) const ENTRY_LEN: usize = 0x0C;
pub(crate) const DIRECTORY_FLAG: u32 = 0xFF00_0000;
pub(crate) const NAME_MASK: u32 = 0x00FF_FFFF;
/// What the mastering put in a directory's flag byte. The reader takes any
/// nonzero one, a writer has to pick.
const DIRECTORY_TYPE: u32 = 0x0100_0000;

/// The project directory the file table covers.
pub(crate) const ROOT: &str = "files";

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

/// Reads one name out of the pool. Shift-JIS, following the archives.
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

/// A file table built out of a project tree, with nowhere for the files to go
/// yet.
pub(crate) struct Table {
    /// The table itself. Every file's data offset is still zero, since the
    /// layout is worked out from how long this came to.
    pub(crate) bytes: Writer,
    /// What it holds, in table order, at offsets nothing has assigned yet.
    pub(crate) entries: Vec<Entry>,
    /// Where each file's data offset goes, in the order the files appear in
    /// `entries`.
    pub(crate) offsets: Vec<usize>,
}

/// Builds the file table for a project tree.
///
/// Entries go depth first, each directory's children ordered by their
/// uppercased name, and the name pool follows in that same order, back to back
/// and NUL terminated. The root has no name of its own: its offset is left at
/// 0, which is where the first entry's name starts, and nothing reads it.
/// The uppercasing matters: ordering on the raw bytes or comparing
/// case-insensitively puts `_` on the other side of the letters and lays the
/// table out differently. Two siblings sharing an uppercased name are refused,
/// so the order is total.
pub(crate) fn build(items: &[&Item]) -> Result<Table> {
    // Everything the tree is, keyed by the path of the directory holding it.
    // The paths come in flat, so this is what puts them back together.
    let mut children: HashMap<&str, Vec<Child>> = HashMap::new();
    for item in items {
        let path = item.path();
        if path == ROOT {
            continue;
        }
        let (parent, name) = path
            .rsplit_once('/')
            .ok_or_else(|| Error::Orphan(path.to_string()))?;
        let name = name_bytes(name)?;
        children.entry(parent).or_default().push(Child {
            upper: name.to_ascii_uppercase(),
            name,
            item,
        });
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|a, b| a.upper.cmp(&b.upper));

        // The game compares names through tolower (`isSame` in dvdfs.c), so
        // two siblings apart only in case would shadow each other on the
        // console, and their order here would be luck besides.
        for pair in siblings.windows(2) {
            if pair[0].upper == pair[1].upper {
                return Err(Error::NameClash(
                    pair[0].item.path().to_string(),
                    pair[1].item.path().to_string(),
                ));
            }
        }
    }

    // The root covers the whole table, so its end index is not known until
    // everything under it has been laid down.
    let mut nodes = vec![Node {
        path: String::new(),
        name: Vec::new(),
        name_offset: 0,
        kind: Kind::Directory { parent: 0, end: 0 },
    }];
    let mut pool = 0;
    let mut walked = HashSet::new();
    push(&children, ROOT, 0, &mut nodes, &mut pool, &mut walked)?;
    nodes[0].kind = Kind::Directory {
        parent: 0,
        end: nodes.len() as u32,
    };

    // A directory that never came up in the walk is one whose own directory is
    // not in the tree, so its contents would have been dropped silently.
    for item in items {
        let path = item.path();
        let above = path.rsplit_once('/').map_or(ROOT, |(parent, _)| parent);
        if path != ROOT && !walked.contains(above) {
            return Err(Error::Orphan(path.to_string()));
        }
    }

    Ok(emit(nodes))
}

/// Lays the nodes out: the array, then the names in the same order.
fn emit(nodes: Vec<Node>) -> Table {
    let pool: usize = nodes.iter().skip(1).map(|node| node.name.len() + 1).sum();
    let mut bytes = Writer::with_capacity(nodes.len() * ENTRY_LEN + pool);
    let mut entries = Vec::with_capacity(nodes.len() - 1);
    let mut offsets = Vec::new();

    for node in &nodes {
        match node.kind {
            Kind::Directory { parent, end } => {
                bytes.u32(DIRECTORY_TYPE | node.name_offset);
                bytes.u32(parent);
                bytes.u32(end);
            }
            Kind::File { size } => {
                bytes.u32(node.name_offset);
                offsets.push(bytes.len());
                bytes.u32(0);
                bytes.u32(size);
            }
        }
    }
    // The root is not named here, so the pool opens with the first entry's
    // name, which is what the root's own offset of 0 lands on.
    for node in nodes.iter().skip(1) {
        bytes.bytes(&node.name);
        bytes.u8(0);
    }

    // The root is the table itself rather than something in it, so it is not
    // one of the entries a disc reports.
    for node in nodes.into_iter().skip(1) {
        entries.push(match node.kind {
            Kind::Directory { .. } => Entry::Directory { path: node.path },
            Kind::File { size } => Entry::File {
                path: node.path,
                offset: 0,
                size: size as u64,
            },
        });
    }

    Table {
        bytes,
        entries,
        offsets,
    }
}

/// Walks one directory, appending its contents and then whatever they hold.
fn push<'a>(
    children: &HashMap<&'a str, Vec<Child<'a>>>,
    directory: &'a str,
    parent: u32,
    nodes: &mut Vec<Node>,
    pool: &mut u32,
    walked: &mut HashSet<&'a str>,
) -> Result<()> {
    walked.insert(directory);
    let Some(siblings) = children.get(directory) else {
        return Ok(());
    };

    for child in siblings {
        // The name's position in the pool shares a word with the flag byte, so
        // there is a limit to how much of it a table can address.
        let name_offset = *pool;
        if name_offset > NAME_MASK {
            return Err(Error::TooManyNames);
        }
        *pool += child.name.len() as u32 + 1;

        let at = nodes.len();
        match child.item {
            Item::Directory { path } => {
                nodes.push(Node {
                    path: path.clone(),
                    name: child.name.clone(),
                    name_offset,
                    kind: Kind::Directory { parent, end: 0 },
                });
                push(children, path, at as u32, nodes, pool, walked)?;

                // Where the subtree ends, which is only known now it is over.
                nodes[at].kind = Kind::Directory {
                    parent,
                    end: nodes.len() as u32,
                };
            }
            Item::File { path, size } => nodes.push(Node {
                path: path.clone(),
                name: child.name.clone(),
                name_offset,
                // Sizes are checked against the end of the user area before a
                // table is ever built, so none of them is wider than a field.
                kind: Kind::File { size: *size as u32 },
            }),
        }
    }
    Ok(())
}

/// Encodes a name back to the Shift-JIS the pool holds, having first refused
/// anything the walk above would not have handed out.
fn name_bytes(name: &str) -> Result<Vec<u8>> {
    if name.is_empty()
        || name.contains(['/', '\\'])
        || name.chars().all(|c| c == '.')
        || name.chars().any(char::is_control)
    {
        return Err(Error::UnwritableName(name.to_string()));
    }

    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(name);
    match unmappable {
        true => Err(Error::UnwritableName(name.to_string())),
        false => Ok(bytes.into_owned()),
    }
}

/// One thing in a directory, with its name already in the form the table sorts
/// and stores it in.
struct Child<'a> {
    name: Vec<u8>,
    upper: Vec<u8>,
    item: &'a Item,
}

/// One node on its way into the table, before it has an index or a place on
/// the disc.
struct Node {
    path: String,
    name: Vec<u8>,
    name_offset: u32,
    kind: Kind,
}

enum Kind {
    Directory { parent: u32, end: u32 },
    File { size: u32 },
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
