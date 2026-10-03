//! What every binary format crate is built on: reading and writing a file's
//! bytes, and telling which format a file is.
//!
//! Everything on the disc is big-endian, and every format crate parses the same
//! shape: read a header, follow an offset into a table, read records at
//! computed positions. A [`Reader`] does that over a borrowed buffer, and a
//! [`Writer`] builds one up. Both are bounds-checked.
//!
//! Each format crate implements [`Format`] on its decoded struct, and every
//! file's opening magic lives in [`FileKind`].

mod format;
mod layout;
mod reader;
mod writer;

pub use format::{Checked, FileKind, Format, WrongKind};
pub use layout::{Be16, Be32, Flag, Layout, bytes_of, view_at_mut};
pub use reader::Reader;
pub use writer::Writer;

/// A read that could not be satisfied from the buffer it was aimed at.
///
/// Every offset here comes out of a file header, which is to say out of a file
/// somebody else wrote, so all of them land in this type rather than in a
/// panic.
#[derive(Debug, thiserror::Error)]
pub enum ByteError {
    #[error("read of {len} bytes at {pos:#x} runs past the end of a {size:#x} byte buffer")]
    OutOfBounds { pos: usize, len: usize, size: usize },

    #[error("the string at {pos:#x} is not terminated before the end of the buffer")]
    Unterminated { pos: usize },
}

pub type Result<T> = std::result::Result<T, ByteError>;

/// Borrows `len` bytes of `data` at `pos`, the one bounds check every read
/// goes through.
fn slice_at(data: &[u8], pos: usize, len: usize) -> Result<&[u8]> {
    let out_of_bounds = || ByteError::OutOfBounds {
        pos,
        len,
        size: data.len(),
    };
    let end = pos.checked_add(len).ok_or_else(out_of_bounds)?;
    data.get(pos..end).ok_or_else(out_of_bounds)
}

#[cfg(test)]
layout! {
    /// Fields of every width, so a view at an odd position proves nothing
    /// needed alignment.
    struct Record {
        tag: u8,
        wide: Be32,
        narrow: Be16,
    }
}
