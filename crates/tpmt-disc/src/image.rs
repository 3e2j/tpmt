//! The file a disc is read out of, whether or not it is the disc itself.
//!
//! Reads are positional and take `&self`, so threads can pull disjoint regions
//! from one handle at once. Unpack is parallel and depends on that.

use std::fs::File;
use std::io;

use crate::{Error, Result, ciso};

/// Where an offset on the disc lands in the file holding it.
enum Layout {
    /// The file is the image.
    Raw,
    /// The image is inside a container, so an offset has to be looked up rather
    /// than seeked to.
    Ciso(ciso::Map),
}

/// A file that several threads read from at once.
///
/// Unix reads at an offset without touching the shared file position. Windows
/// has no such call: its positional read moves the position as a side effect,
/// so concurrent reads would land on top of each other and a lock is the price
/// of the same guarantee.
pub(crate) struct Handle {
    /// Length of the image, which is not the length of the file when the file
    /// is a container.
    pub(crate) len: u64,
    layout: Layout,
    #[cfg(unix)]
    file: File,
    #[cfg(not(unix))]
    file: std::sync::Mutex<File>,
}

impl Handle {
    /// Opens a file, looking inside it first in case it holds an image rather
    /// than being one.
    pub(crate) fn open(file: File, len: u64) -> Result<Self> {
        let handle = Self::raw(file, len);
        if read_at(&handle, 0, ciso::MAGIC.len() as u64)? != ciso::MAGIC {
            return Ok(handle);
        }

        let map = ciso::Map::read(&read_at(&handle, 0, ciso::HEADER_LEN)?, len)?;
        Ok(Self {
            len: map.image_len(),
            layout: Layout::Ciso(map),
            ..handle
        })
    }

    #[cfg(unix)]
    const fn raw(file: File, len: u64) -> Self {
        Self {
            len,
            layout: Layout::Raw,
            file,
        }
    }

    #[cfg(not(unix))]
    fn raw(file: File, len: u64) -> Self {
        Self {
            len,
            layout: Layout::Raw,
            file: std::sync::Mutex::new(file),
        }
    }

    /// Reads from the image, block by block if the file is a container.
    fn read(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let Layout::Ciso(map) = &self.layout else {
            return self.read_file(buf, offset);
        };

        let mut done = 0;
        while done < buf.len() {
            let (at, run) = map.locate(offset + done as u64);
            let take = run.min((buf.len() - done) as u64) as usize;
            let part = &mut buf[done..done + take];

            match at {
                Some(at) => self.read_file(part, at)?,
                // A block nobody stored is a block that held nothing.
                None => part.fill(0),
            }
            done += take;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn read_file(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.read_exact_at(buf, offset)
    }

    #[cfg(not(unix))]
    fn read_file(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = self.file.lock().expect("a disc read panicked mid-read");
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }
}

pub(crate) fn read_at(handle: &Handle, offset: u64, len: u64) -> Result<Vec<u8>> {
    // Checked before the buffer is allocated, not by the read itself. Lengths
    // come out of the file table, and a corrupt one asks for up to 4 GB before
    // the read that would have refused it ever runs.
    let past_the_end = |end: u64| end > handle.len;
    if offset.checked_add(len).is_none_or(past_the_end) {
        return Err(Error::Read {
            offset,
            len,
            source: io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the image is only {:#x} bytes long", handle.len),
            ),
        });
    }

    let mut buf = vec![0u8; len as usize];
    handle
        .read(&mut buf, offset)
        .map_err(|source| Error::Read {
            offset,
            len,
            source,
        })?;
    Ok(buf)
}
