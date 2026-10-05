use std::fmt;

use crate::{ByteError, Result, bytes_at};

/// A type whose memory layout is its file layout.
///
/// [`Reader::record_at`](crate::Reader::record_at) can borrow one straight out of
/// the buffer, and [`Writer::record`](crate::Writer::record) can append one as
/// it stands.
///
/// Every record is built from these, and only these:
///
/// - `u8`
/// - [`Be16`], a big-endian `u16`
/// - [`Be32`], a big-endian `u32`
/// - [`Flag`], a byte read as a `bool`
/// - `[T; N]` of any of the above
/// - another struct declared with [`record!`](crate::record!)
///
/// Define records with [`record!`](crate::record!), which checks every field
/// and writes the impl, rather than implementing this by hand.
///
/// # Safety
///
/// The implementor **must**:
///
/// - be `#[repr(C)]` or `#[repr(transparent)]`, so fields keep declaration
///   order and add no padding;
/// - hold only `Record` fields, so it has alignment 1 and every bit pattern
///   is valid.
pub unsafe trait Record: Sized {
    /// How many bytes the record takes in the file.
    const LEN: usize = size_of::<Self>();

    /// The record exactly as the file stores it.
    fn as_bytes(&self) -> &[u8] {
        bytes_of(std::slice::from_ref(self))
    }

    /// Reads the same bytes as another record of the same length, for a
    /// table whose records are laid out one of several ways. A length that
    /// differs fails to compile.
    ///
    /// ```compile_fail
    /// use tpmt_binary::{Be16, Be32, Record};
    ///
    /// let wide = Be32::new(0);
    /// let narrow: &Be16 = wide.cast();
    /// ```
    fn cast<U: Record>(&self) -> &U {
        const {
            assert!(
                size_of::<Self>() == size_of::<U>(),
                "a cast keeps the length"
            );
        }
        const { assert!(align_of::<U>() == 1, "a record must be aligned to 1") };
        // SAFETY: `U` is exactly as long as `self` (checked above) and aligned
        // to 1, so any address suits it, and `Record` promises every bit
        // pattern is a valid `U`.
        unsafe { &*std::ptr::from_ref(self).cast::<U>() }
    }
}

// SAFETY: one byte, and every value is valid.
unsafe impl Record for u8 {}
// SAFETY: an array of align-1 elements with no padding has none either.
unsafe impl<T: Record, const N: usize> Record for [T; N] {}

/// Defines a `#[repr(C)]` struct and implements [`Record`] for it. A field
/// whose type is not `Record`, such as a `bool` or a native `u32`, fails to
/// compile.
///
/// ```
/// tpmt_binary::record! {
///     pub struct Header {
///         pub magic: [u8; 4],
///         pub size: tpmt_binary::Be32,
///     }
/// }
/// ```
///
/// ```compile_fail
/// tpmt_binary::record! {
///     struct Flagged {
///         set: bool,
///     }
/// }
/// ```
#[macro_export]
macro_rules! record {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $($(#[$field_meta:meta])* $field_vis:vis $field:ident: $ty:ty),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[repr(C)]
        $vis struct $name {
            $($(#[$field_meta])* $field_vis $field: $ty),*
        }

        const _: () = {
            const fn field<T: $crate::Record>() {}
            $(field::<$ty>();)*
        };

        // SAFETY: repr(C), and every field is `Record`, checked above.
        unsafe impl $crate::Record for $name {}
    };
}

/// A run of records exactly as the file stores it.
#[must_use]
pub const fn bytes_of<T: Record>(records: &[T]) -> &[u8] {
    const { assert!(align_of::<T>() == 1, "a record must be aligned to 1") };
    // SAFETY: `Record` rules out padding, so every byte behind `records` is
    // initialized, and a `u8` slice needs no alignment.
    unsafe { std::slice::from_raw_parts(records.as_ptr().cast::<u8>(), size_of_val(records)) }
}

/// Borrows a record at an absolute position in `data`.
pub fn record_at<T: Record>(data: &[u8], pos: usize) -> Result<&T> {
    const { assert!(align_of::<T>() == 1, "a record must be aligned to 1") };
    let bytes = bytes_at(data, pos, T::LEN)?;
    // SAFETY: `bytes` is exactly `size_of::<T>()` long and borrowed for as
    // long as the result. `T` is aligned to 1 (checked above), so any address
    // suits it, and `Record` promises every bit pattern is a valid `T`.
    Ok(unsafe { &*bytes.as_ptr().cast::<T>() })
}

/// Borrows `count` records laid end to end at an absolute position in `data`.
pub fn records_at<T: Record>(data: &[u8], pos: usize, count: usize) -> Result<&[T]> {
    const { assert!(align_of::<T>() == 1, "a record must be aligned to 1") };
    let len = count.checked_mul(T::LEN).ok_or(ByteError::OutOfBounds {
        pos,
        len: usize::MAX,
        size: data.len(),
    })?;
    let bytes = bytes_at(data, pos, len)?;
    // SAFETY: `bytes` is exactly `count * size_of::<T>()` long and borrowed
    // for as long as the result. `T` is aligned to 1 (checked above), so any
    // address suits it, and `Record` promises every bit pattern is a valid
    // `T`.
    Ok(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<T>(), count) })
}

/// Borrows a record at an absolute position for writing, so a field can be
/// changed in place without working out its byte offset.
///
/// # Errors
///
/// Returns [`ByteError::OutOfBounds`] if the record runs past the end of
/// `data`.
pub fn record_at_mut<T: Record>(data: &mut [u8], pos: usize) -> Result<&mut T> {
    const { assert!(align_of::<T>() == 1, "a record must be aligned to 1") };
    let size = data.len();
    let bytes = pos
        .checked_add(T::LEN)
        .and_then(|end| data.get_mut(pos..end))
        .ok_or(ByteError::OutOfBounds {
            pos,
            len: T::LEN,
            size,
        })?;
    // SAFETY: `bytes` is exactly `size_of::<T>()` long and borrowed mutably
    // for as long as the result. `T` is aligned to 1 (checked above), so any
    // address suits it, and `Record` promises every bit pattern is a valid
    // `T`, so whatever is written through it leaves valid bytes behind.
    Ok(unsafe { &mut *bytes.as_mut_ptr().cast::<T>() })
}

/// A big-endian `u16` as it sits in a file: two bytes, aligned to one.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Be16([u8; 2]);

impl Be16 {
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value.to_be_bytes())
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        u16::from_be_bytes(self.0)
    }
}

impl fmt::Debug for Be16 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#X}", self.get())
    }
}

// SAFETY: repr(transparent) over a byte array.
unsafe impl Record for Be16 {}

/// A big-endian `u32` as it sits in a file: four bytes, aligned to one.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Be32([u8; 4]);

impl Be32 {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value.to_be_bytes())
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
}

impl fmt::Debug for Be32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#X}", self.get())
    }
}

// SAFETY: repr(transparent) over a byte array.
unsafe impl Record for Be32 {}

/// A one-byte flag as it sits in a file. Any byte is a valid `Flag`, unlike
/// a `bool`, and any nonzero byte reads as set.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct Flag(u8);

impl Flag {
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self(value as u8)
    }

    #[must_use]
    pub const fn get(self) -> bool {
        self.0 != 0
    }
}

impl fmt::Debug for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

// SAFETY: repr(transparent) over a byte, and `Flag` gives every value a
// meaning.
unsafe impl Record for Flag {}

#[cfg(test)]
record! {
    /// Fields of every width, so a record at an odd position proves nothing
    /// needed alignment. Shared by the tests of every module here.
    pub struct Sample {
        pub tag: u8,
        pub wide: Be32,
        pub narrow: Be16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    record! {
        struct Split {
            head: [u8; 3],
            tail: Be32,
        }
    }

    #[test]
    fn a_cast_reads_the_same_bytes() {
        let record = Sample {
            tag: 0x0D,
            wide: Be32::new(0x0001_0203),
            narrow: Be16::new(0xACED),
        };
        let split: &Split = record.cast();
        assert_eq!(split.head, [0x0D, 0x00, 0x01]);
        assert_eq!(split.tail.get(), 0x0203_ACED);
    }

    /// Starting at an odd position proves the record never needed alignment.
    #[test]
    fn a_field_edited_in_place_reads_back() {
        let mut data = [0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED];
        record_at_mut::<Sample>(&mut data, 1).unwrap().narrow = Be16::new(0xBEEF);
        assert_eq!(data, [0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xBE, 0xEF]);
        assert!(matches!(
            record_at_mut::<Sample>(&mut data, 2),
            Err(ByteError::OutOfBounds { .. })
        ));
        assert!(record_at_mut::<Sample>(&mut data, usize::MAX).is_err());
    }
}
