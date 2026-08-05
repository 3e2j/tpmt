//! Reading and writing GameCube disc images.
//!
//! Raw disc dumps, distributed as `.iso`, or other variants, and CISO
//! containers, which hold one with the empty blocks left out.
//!
//! A disc is a fixed-layout preamble (boot header, disc metadata, apploader,
//! executable) followed by a file string table describing everything else as a
//! directory tree.
//!
//! Also identifies a disc. The game id and revision come out of the boot
//! header, and a SHA-1 over the whole image says whether this is the same dump
//! a project was unpacked from.
//!
//! Hands out byte ranges. Decoding the file contents is the job of the format
//! crates.
//!
//! Reads are positional and take `&self`, so threads can pull disjoint regions
//! from one handle at once. Unpack is parallel and depends on that.

// Assumed: the mastering fill between the file table and the first file holds
// nothing. It measures as uniform random and nothing on the disc points at it.
// The game reads only through the file table: the decompilation has no
// absolute-offset read in it, and the audio streaming library, the one
// documented way around the file table, is not linked into any of the three
// prints. So the fill is not reported here and a build will not have it, which
// costs a matching hash and saves 400 MB.

// TODO: the SHA-1 that identifies a dump. It belongs with the project file that
// records it, and nothing writes one yet.

// TODO: writing. Building an image back is the other half of this crate. A CISO
// reads today but only an ISO will come back out.

// TODO: Wii discs, out of scope. Recognised by their magic only so the Wii
// print of this game gets a real answer instead of being called unreadable.

// TODO: multiboot GameCube discs (`GCOPDV`, `COBRAM`, `GGCOSD`, `RGCOSD`) carry
// several games behind a partition table at 0x40. Twilight Princess is not one,
// so they read as an ordinary disc and only the partition at 0 is seen.

mod ciso;
mod fst;
mod image;
mod sys;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::image::{Handle, read_at};

pub use crate::sys::{Bi2, Boot, Metadata};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{}`: {source}", .path.display())]
    Open { path: PathBuf, source: io::Error },

    #[error("read of {len} bytes at {offset:#x}: {source}")]
    Read {
        offset: u64,
        len: u64,
        source: io::Error,
    },

    #[error("not a GameCube disc image")]
    NotADisc,

    #[error("this is a Wii disc image, which is not supported yet")]
    WiiDisc,

    #[error("the disc's boot header is corrupt: {0}")]
    CorruptHeader(&'static str),

    #[error("the disc's file table is corrupt: {0}")]
    CorruptFileTable(&'static str),

    #[error(
        "{region} holds data at {offset:#x}, outside its known fields, which a build would lose"
    )]
    UnknownPreambleData { region: &'static str, offset: u64 },

    #[error(
        "{what} is {found:#x} where a build would put {want:#x}, so rebuilding would change it"
    )]
    DerivedValueDiffers {
        what: &'static str,
        found: u32,
        want: u32,
    },

    #[error(transparent)]
    Bytes(#[from] tpmt_bytes::ByteError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One thing the disc holds, at the path it will be unpacked to.
///
/// Everything is one of these, the preamble included, so a caller can walk the
/// whole disc as a flat list. They come back in disc order: the preamble, then
/// the file table depth first, a directory ahead of its contents.
#[derive(Debug, Clone)]
pub enum Entry {
    /// A byte range on the disc.
    File {
        path: String,
        offset: u64,
        size: u64,
    },
    /// A directory, holding no bytes of its own. Listed so an empty one is not
    /// lost on the way out.
    Directory { path: String },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Directory { path } => path,
        }
    }
}

/// An open disc image.
pub struct Disc {
    handle: Handle,
    metadata: Metadata,
}

impl Disc {
    /// Opens an image, or a container holding one.
    pub fn open(path: &Path) -> Result<Self> {
        let open = |source| Error::Open {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(open)?;
        let len = file.metadata().map_err(open)?.len();

        let handle = Handle::open(file, len)?;
        let boot = read_at(&handle, 0, sys::BOOT_LEN)?;
        sys::identify(&boot)?;

        // The apploader's length is one of the values the header is checked
        // against, so it has to be in hand before the header can be read.
        let apploader = read_at(&handle, sys::APPLOADER_OFFSET, sys::APPLOADER_HEADER_LEN)?;
        let bi2 = read_at(&handle, sys::BI2_OFFSET, sys::BI2_LEN)?;
        let metadata = Metadata {
            boot: sys::boot(&boot, sys::apploader_len(&apploader)?)?,
            bi2: sys::bi2(&bi2)?,
        };
        Ok(Self { handle, metadata })
    }

    /// What the preamble records that a build cannot work out again.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The length of the image, which is not the length of the file it came out
    /// of when that file is a container.
    pub fn len(&self) -> u64 {
        self.handle.len
    }

    pub fn is_empty(&self) -> bool {
        self.handle.len == 0
    }

    /// Reads a range of the disc.
    ///
    /// Positional, so several threads can pull from one open disc at once.
    pub fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        read_at(&self.handle, offset, len)
    }

    /// Everything the disc holds: the preamble under `sys/`, then the game's
    /// own files and directories under `files/`, in file table order.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut entries = sys::entries(self)?;
        entries.extend(self.file_entries()?);
        Ok(entries)
    }

    fn file_entries(&self) -> Result<Vec<Entry>> {
        let boot = self.read(0, sys::BOOT_LEN)?;
        let (offset, size) = sys::fst_range(&boot)?;
        fst::walk(&self.read(offset, size)?)
    }
}
