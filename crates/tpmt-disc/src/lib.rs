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
// documented way around the file table, is not linked into the game. So the
// fill is not reported here and a build will not have it, which
// costs a matching hash and saves 400 MB.

// TODO: CISO out, out of scope. Containers are read because that is how dumps
// arrive, and nothing on the other side wants one back, so an image only ever
// comes out as a raw disc.

// TODO: Wii discs, out of scope. Recognised by their magic only so the Wii
// print of this game gets a real answer instead of being called unreadable.

// TODO: multiboot GameCube discs (`GCOPDV`, `COBRAM`, `GGCOSD`, `RGCOSD`) carry
// several games behind a partition table at 0x40. Twilight Princess is not one,
// so they read as an ordinary disc and only the partition at 0 is seen.

mod ciso;
mod fst;
mod image;
mod sys;
mod write;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::image::{Handle, read_at};

pub use crate::sys::{BI2_PATH, BOOT_PATH, Bi2, Boot, Metadata, bi2_bin, boot_bin_over};
pub use crate::write::{Image, Layout};

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

    #[error("write of {len} bytes at {offset:#x}: {source}")]
    Write {
        offset: u64,
        len: u64,
        source: io::Error,
    },

    #[error("the disc's {0}")]
    Unwritable(&'static str),

    #[error("`{0}` is not a name a file table can hold")]
    UnwritableName(String),

    #[error("`{0}` and `{1}` are the same name to the game, which reads without case")]
    NameClash(String, String),

    #[error("`{0}` sits in a directory the project does not have")]
    Orphan(String),

    #[error("there are more file names than a file table can address")]
    TooManyNames,

    #[error("nothing on a disc corresponds to `{0}`")]
    UnknownEntry(String),

    #[error("the project has no `{0}`, and a disc does not boot without one")]
    MissingEntry(&'static str),

    #[error("the files come to {len:#x} bytes, where a disc holds {end:#x}")]
    TooLarge { len: u64, end: u64 },

    #[error("`{path}` is {found:#x} bytes where the layout reserved {want:#x}")]
    WrongSize { path: String, found: u64, want: u64 },

    #[error("the image was handed {0}")]
    Mismatch(&'static str),

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

/// One thing to put on a disc, at the path it was edited at.
///
/// An `Entry` without the offset, which is the one thing about a disc that a
/// build works out rather than being told.
#[derive(Debug, Clone)]
pub enum Item {
    File { path: String, size: u64 },
    Directory { path: String },
}

impl Item {
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
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The boot header exactly as this disc holds it.
    ///
    /// Neither of these is a file a project keeps, so this is the only thing a
    /// rebuilt one can be held against to say whether it came out different.
    pub fn boot_bin(&self) -> Result<Vec<u8>> {
        self.read(0, sys::BOOT_LEN)
    }

    /// The disc metadata exactly as this disc holds it.
    pub fn bi2_bin(&self) -> Result<Vec<u8>> {
        self.read(sys::BI2_OFFSET, sys::BI2_LEN)
    }

    /// The length of the image, which is not the length of the file it came out
    /// of when that file is a container.
    pub const fn len(&self) -> u64 {
        self.handle.len
    }

    pub const fn is_empty(&self) -> bool {
        self.handle.len == 0
    }

    /// Reads a range of the disc.
    ///
    /// Positional, so several threads can pull from one open disc at once.
    pub fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        read_at(&self.handle, offset, len)
    }

    /// The SHA-1 of the image, which is what says whether a project's source
    /// disc is still the same dump it was unpacked from.
    ///
    /// Over the image rather than the file, so a container and a raw dump of
    /// the same disc answer the same.
    pub fn sha1(&self) -> Result<String> {
        const CHUNK: u64 = 1 << 20;

        let mut hash = Sha1::new();
        let mut at = 0;
        while at < self.handle.len {
            let take = CHUNK.min(self.handle.len - at);
            hash.update(&self.read(at, take)?);
            at += take;
        }
        Ok(format!("{:x}", hash.finalize()))
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
