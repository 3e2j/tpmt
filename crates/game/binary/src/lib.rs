//! What every binary format crate is built on.
//!
//! It does three things:
//! - Read big-endian bytes at a cursor or absolute position via [`Reader`]
//! - Appends big-endian bytes via [`Writer`]
//! - host a [`Format`] trait for formats to implement.
//!
//! Every opening magic lives in [`FileKind`], which checks and identifies them.
//!
//! Declaring structs with [`record!`] is shorthand for repr(C), letting it be
//! read/written exactly how it was laid out in memory. The types its fields can
//! be are listed on [`Record`].

mod format;
mod kind;
mod reader;
mod record;
mod writer;

pub use format::{Checked, Format};
pub use kind::{FileKind, WrongKind};
pub use reader::Reader;
pub use record::{Be16, Be32, Flag, Record, bytes_of, record_at_mut};
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
fn bytes_at(data: &[u8], pos: usize, len: usize) -> Result<&[u8]> {
    let out_of_bounds = || ByteError::OutOfBounds {
        pos,
        len,
        size: data.len(),
    };
    let end = pos.checked_add(len).ok_or_else(out_of_bounds)?;
    data.get(pos..end).ok_or_else(out_of_bounds)
}
