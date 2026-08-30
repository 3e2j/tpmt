//! Laying a disc out and writing it.
//!
//! What goes where is not recorded anywhere on a disc, so it is worked out
//! again from the same rules the reader checks the original against: the
//! executable and the file table each on the first 0x100 boundary past what
//! came before them, user data on the first 0x8000 past the file table, and the
//! files themselves packed in table order on 4 byte boundaries.
//!
//! The mastering fill a retail disc opens its user area with is not put back,
//! so an image comes out around 400 MB shorter than the disc it was unpacked
//! from. Nothing reads the fill; see the note at the top of the crate.
//!
//! Two steps, because the caller has 1.4 GB of files and no reason to hold them
//! all at once. A layout says where everything goes, and then the image is fed
//! one file at a time, in that order.

use std::io::Write;

use sha1::{Digest, Sha1};

use crate::{Entry, Error, Item, Metadata, Result, fst, sys};

/// A disc laid out but not yet written.
pub struct Layout {
    entries: Vec<Entry>,
    /// The three pieces nobody hands over, each at the offset it goes to: the
    /// boot header, the disc metadata, and the file table.
    generated: Vec<(u64, Vec<u8>)>,
    len: u64,
}

impl Layout {
    /// Works out where everything goes.
    ///
    /// `items` is the whole project: the two preamble files under `sys/`, and
    /// the game's own files and directories under `files/`. Their order does
    /// not matter, since the file table has an order of its own.
    pub fn plan(metadata: &Metadata, items: &[Item]) -> Result<Self> {
        let mut apploader = None;
        let mut dol = None;
        let mut files = Vec::new();
        let mut total = 0u64;

        for item in items {
            let path = item.path();
            let size = match item {
                Item::File { size, .. } => *size,
                Item::Directory { .. } => 0,
            };
            total += size;

            match path {
                sys::APPLOADER_PATH => apploader = Some(size),
                sys::DOL_PATH => dol = Some(size),
                _ if inside_the_tree(path) => files.push(item),
                _ => return Err(Error::UnknownEntry(path.to_string())),
            }
        }

        let apploader = apploader.ok_or(Error::MissingEntry(sys::APPLOADER_PATH))?;
        let dol = dol.ok_or(Error::MissingEntry(sys::DOL_PATH))?;

        // Ahead of anything else, so that everything below can be a u32 field
        // without arithmetic that could wrap one.
        let end = u64::from(sys::USER_AREA_END);
        if total > end {
            return Err(Error::TooLarge { len: total, end });
        }

        let mut table = fst::build(&files)?;
        let dol_offset = (sys::APPLOADER_OFFSET + apploader).next_multiple_of(sys::PREAMBLE_ALIGN);
        let fst_offset = (dol_offset + dol).next_multiple_of(sys::PREAMBLE_ALIGN);
        let fst_len = table.bytes.len() as u64;
        if fst_offset + fst_len > end {
            return Err(Error::TooLarge {
                len: fst_offset + fst_len,
                end,
            });
        }

        // Packed from the start of the user area in table order, each file on a
        // 4 byte boundary. Nothing is left between them.
        let mut at = u64::from(sys::user_position(fst_offset as u32, fst_len as u32));
        let mut entries = vec![
            Entry::File {
                path: sys::APPLOADER_PATH.to_string(),
                offset: sys::APPLOADER_OFFSET,
                size: apploader,
            },
            Entry::File {
                path: sys::DOL_PATH.to_string(),
                offset: dol_offset,
                size: dol,
            },
        ];
        let mut offsets = table.offsets.iter();

        // Where the last file ends, which is where the image does. The position
        // above runs on past it to the next boundary.
        let mut last = at;

        for entry in &mut table.entries {
            let Entry::File { offset, size, .. } = entry else {
                continue;
            };
            *offset = at;
            last = at + *size;
            at = last.next_multiple_of(FILE_ALIGN);
            if last > end {
                return Err(Error::TooLarge { len: last, end });
            }

            let field = offsets.next().expect("every file reserved an offset");
            table.bytes.u32_at(*field, *offset as u32);
        }
        entries.append(&mut table.entries);

        let boot = sys::boot_bin(
            &metadata.boot,
            &sys::BootLayout {
                apploader_len: apploader as u32,
                dol_offset: dol_offset as u32,
                fst_offset: fst_offset as u32,
                fst_len: fst_len as u32,
            },
        )?;
        Ok(Self {
            entries,
            generated: vec![
                (0, boot),
                (sys::BI2_OFFSET, sys::bi2_bin(&metadata.bi2)),
                (fst_offset, table.bytes.finish()),
            ],
            len: last,
        })
    }

    /// Everything the image will hold, in the order it holds it, which is also
    /// the order its files have to be handed over in.
    ///
    /// The same list, at the same offsets, that reading the finished image back
    /// reports.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// How long the image comes out.
    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Starts writing the image out.
    pub fn write<W: Write>(&self, out: W) -> Image<'_, W> {
        Image {
            layout: self,
            out,
            at: 0,
            next: 0,
            generated: 0,
            hash: Sha1::new(),
        }
    }
}

/// Files sit on one of these, and mostly back to back.
const FILE_ALIGN: u64 = 0x04;

/// Whether a path is somewhere in the file table's tree, rather than one of the
/// preamble files or something a disc has no room for at all.
fn inside_the_tree(path: &str) -> bool {
    path.strip_prefix(fst::ROOT)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// An image being written out.
///
/// The caller hands over one file at a time, in the order the layout put them,
/// and everything in between is filled in here: the boot header, the disc
/// metadata, the file table, and zeros over the gaps alignment leaves.
///
/// Runs forward and never seeks, so the output is only ever appended to and a
/// hash of it falls out on the way past.
pub struct Image<'a, W: Write> {
    layout: &'a Layout,
    out: W,
    at: u64,
    next: usize,
    generated: usize,
    hash: Sha1,
}

impl<W: Write> Image<'_, W> {
    /// Writes the next file the layout holds.
    pub fn file(&mut self, bytes: &[u8]) -> Result<()> {
        let layout = self.layout;
        let file = loop {
            let entry = layout
                .entries
                .get(self.next)
                .ok_or(Error::Mismatch("more files than the layout holds"))?;
            self.next += 1;
            if let Entry::File { path, offset, size } = entry {
                break (path, *offset, *size);
            }
        };

        let (path, offset, size) = file;
        if bytes.len() as u64 != size {
            return Err(Error::WrongSize {
                path: path.clone(),
                found: bytes.len() as u64,
                want: size,
            });
        }

        self.pad_to(offset)?;
        self.put(bytes)
    }

    /// Finishes the image, giving back the SHA-1 of everything written, which
    /// is what says later on whether this is the same image.
    pub fn finish(mut self) -> Result<String> {
        let left = self.layout.entries[self.next..]
            .iter()
            .filter(|entry| matches!(entry, Entry::File { .. }))
            .count();
        if left > 0 {
            return Err(Error::Mismatch("fewer files than the layout holds"));
        }

        // A disc with no files at all still has a file table, and this is where
        // it goes out.
        self.pad_to(self.layout.len)?;
        self.out.flush().map_err(|source| Error::Write {
            offset: self.at,
            len: 0,
            source,
        })?;
        Ok(format!("{:x}", self.hash.finalize()))
    }

    /// Runs the image up to a position, laying down whatever the layout put in
    /// between and zeros over the rest.
    fn pad_to(&mut self, offset: u64) -> Result<()> {
        let layout = self.layout;
        while let Some((at, bytes)) = layout.generated.get(self.generated) {
            if *at >= offset {
                break;
            }
            self.zeros(*at)?;
            self.generated += 1;
            self.put(bytes)?;
        }
        self.zeros(offset)
    }

    fn zeros(&mut self, offset: u64) -> Result<()> {
        const NOTHING: [u8; 0x1000] = [0; 0x1000];

        while self.at < offset {
            let take = (offset - self.at).min(NOTHING.len() as u64) as usize;
            self.put(&NOTHING[..take])?;
        }
        Ok(())
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.hash.update(bytes);
        self.out.write_all(bytes).map_err(|source| Error::Write {
            offset: self.at,
            len: bytes.len() as u64,
            source,
        })?;
        self.at += bytes.len() as u64;
        Ok(())
    }
}
