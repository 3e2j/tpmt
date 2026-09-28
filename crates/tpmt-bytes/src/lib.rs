//! Bounds-checked big-endian byte cursors (read/write).
//!
//! Everything on the disc is big-endian, and every format crate parses the same
//! shape: read a header, follow an offset into a table, read records at
//! computed positions. A reader does that over a borrowed buffer; a writer
//! builds one up and backpatches the offsets that were not knowable at the
//! point they were reserved.

use std::fmt;

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

/// A type whose memory layout is its file layout, so [`Reader::view_at`] can
/// borrow one straight out of the buffer and [`Writer::record`] can append
/// one as it stands.
///
/// Implemented for `u8`, `[T; N]` of any `Layout` `T`, [`Be16`], [`Be32`] and
/// [`Flag`]. Define records with [`layout!`], which checks every field and
/// writes the impl, rather than implementing this by hand.
///
/// # Safety
///
/// The implementor **must**:
///
/// - be `#[repr(C)]` or `#[repr(transparent)]`, so fields keep declaration
///   order and add no padding;
/// - hold only `Layout` fields, so it has alignment 1 and every bit pattern
///   is valid.
pub unsafe trait Layout: Sized {
    /// How many bytes the record takes in the file.
    const LEN: usize = size_of::<Self>();
}

// SAFETY: one byte, and every value is valid.
unsafe impl Layout for u8 {}
// SAFETY: an array of align-1 elements with no padding has none either.
unsafe impl<T: Layout, const N: usize> Layout for [T; N] {}
// SAFETY: repr(transparent) over a byte array.
unsafe impl Layout for Be16 {}
// SAFETY: repr(transparent) over a byte array.
unsafe impl Layout for Be32 {}
// SAFETY: repr(transparent) over a byte, and `Flag` gives every value a
// meaning.
unsafe impl Layout for Flag {}

/// Defines a `#[repr(C)]` struct and implements [`Layout`] for it. A field
/// whose type is not `Layout`, such as a `bool` or a native `u32`, fails to
/// compile.
///
/// ```
/// tpmt_bytes::layout! {
///     pub struct Header {
///         pub magic: [u8; 4],
///         pub size: tpmt_bytes::Be32,
///     }
/// }
/// ```
///
/// ```compile_fail
/// tpmt_bytes::layout! {
///     struct Flagged {
///         set: bool,
///     }
/// }
/// ```
#[macro_export]
macro_rules! layout {
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
            const fn field<T: $crate::Layout>() {}
            $(field::<$ty>();)*
        };

        // SAFETY: repr(C), and every field is `Layout`, checked above.
        unsafe impl $crate::Layout for $name {}
    };
}

/// A cursor over a borrowed buffer.
///
/// Sequential reads advance the cursor; the `_at` reads take an absolute
/// position and leave it alone, which is what following an offset out of a
/// header amounts to. Both hand back slices borrowed from the original buffer,
/// so walking a table copies nothing.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Moves the cursor. Landing past the end is not an error until something
    /// is actually read from there.
    pub const fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Borrows `len` bytes at an absolute position.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos..pos + len` runs past the
    /// end of the buffer.
    pub fn slice_at(&self, pos: usize, len: usize) -> Result<&'a [u8]> {
        let out_of_bounds = || ByteError::OutOfBounds {
            pos,
            len,
            size: self.data.len(),
        };
        let end = pos.checked_add(len).ok_or_else(out_of_bounds)?;
        self.data.get(pos..end).ok_or_else(out_of_bounds)
    }

    /// Borrows `len` bytes at the cursor and steps over them.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `len` bytes are not left in the
    /// buffer.
    pub fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let out = self.slice_at(self.pos, len)?;
        // `slice_at` above already proved `self.pos + len` fits in the buffer.
        self.pos = self.pos.saturating_add(len);
        Ok(out)
    }

    /// Reads a fixed-size array at the cursor and steps over it.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let out = self.bytes_at(self.pos)?;
        self.pos = self.pos.saturating_add(N);
        Ok(out)
    }

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if a byte is not left in the buffer.
    pub fn u8(&mut self) -> Result<u8> {
        let [byte] = self.take_array()?;
        Ok(byte)
    }

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if 2 bytes are not left in the buffer.
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take_array()?))
    }

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if 4 bytes are not left in the buffer.
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take_array()?))
    }

    // `at` positions don't silently advance cursor

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos` is not a byte in the buffer.
    pub fn u8_at(&self, pos: usize) -> Result<u8> {
        let [byte] = self.bytes_at(pos)?;
        Ok(byte)
    }

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos..pos + 2` runs past the end
    /// of the buffer.
    pub fn u16_at(&self, pos: usize) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes_at(pos)?))
    }

    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos..pos + 4` runs past the end
    /// of the buffer.
    pub fn u32_at(&self, pos: usize) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes_at(pos)?))
    }

    /// Borrows `N` bytes at an absolute position as a fixed-size array, for a
    /// field with no natural integer width, like a magic or a raw param blob.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos..pos + N` runs past the end
    /// of the buffer.
    pub fn bytes_at<const N: usize>(&self, pos: usize) -> Result<[u8; N]> {
        let bytes = self.slice_at(pos, N)?;
        bytes.try_into().map_err(|_| ByteError::OutOfBounds {
            pos,
            len: N,
            size: self.data.len(),
        })
    }

    /// Borrows a whole record at an absolute position. Its fields decode on
    /// access, and nothing is copied.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if the record runs past the end of
    /// the buffer.
    pub fn view_at<T: Layout>(&self, pos: usize) -> Result<&'a T> {
        const { assert!(align_of::<T>() == 1, "a Layout type must be aligned to 1") };
        let bytes = self.slice_at(pos, size_of::<T>())?;
        // SAFETY: `bytes` is exactly `size_of::<T>()` long and outlives `'a`.
        // `T` is aligned to 1 (checked above), so any address suits it, and
        // `Layout` promises every bit pattern is a valid `T`.
        Ok(unsafe { &*bytes.as_ptr().cast::<T>() })
    }

    /// Borrows `count` records laid end to end at an absolute position, the
    /// way a file stores a table of them.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if the table runs past the end of
    /// the buffer, or its length overflows.
    pub fn slice_of<T: Layout>(&self, pos: usize, count: usize) -> Result<&'a [T]> {
        const { assert!(align_of::<T>() == 1, "a Layout type must be aligned to 1") };
        let len = count
            .checked_mul(size_of::<T>())
            .ok_or(ByteError::OutOfBounds {
                pos,
                len: usize::MAX,
                size: self.data.len(),
            })?;
        let bytes = self.slice_at(pos, len)?;
        // SAFETY: `bytes` is exactly `count * size_of::<T>()` long and outlives
        // `'a`. `T` is aligned to 1 (checked above), so any address suits it,
        // and `Layout` promises every bit pattern is a valid `T`.
        Ok(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<T>(), count) })
    }

    /// Borrows the null-terminated bytes at an absolute position, terminator
    /// excluded. What encoding they are in is the caller's business.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `pos` is past the end of the
    /// buffer, or [`ByteError::Unterminated`] if no null byte follows it.
    pub fn cstr_at(&self, pos: usize) -> Result<&'a [u8]> {
        let rest = self.data.get(pos..).ok_or(ByteError::OutOfBounds {
            pos,
            len: 1,
            size: self.data.len(),
        })?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(ByteError::Unterminated { pos })?;
        rest.get(..end).ok_or(ByteError::Unterminated { pos })
    }
}

/// A buffer being built up.
///
/// Appends run forward from the end, and the `_at` writes take an absolute
/// position and overwrite what is already there, which is how a field reserved
/// before its value was known gets filled in afterwards.
///
/// Nothing here is fallible. The buffer grows to fit whatever is appended, and
/// a position handed to a patch is one the writer gave out earlier rather than
/// an offset read out of somebody else's file.
#[derive(Default)]
pub struct Writer {
    data: Vec<u8>,
}

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a buffer that will not have to grow on the way to `capacity`.
    /// Worth it for the file table, whose length is known before it is built.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
        }
    }

    /// How much has been written, which is also the position the next append
    /// lands at. Reserving a field to backpatch means keeping this first.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
    }

    pub fn u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    /// Appends a whole record, fields in declaration order.
    pub fn record<T: Layout>(&mut self, record: &T) {
        const { assert!(align_of::<T>() == 1, "a Layout type must be aligned to 1") };
        // SAFETY: `Layout` rules out padding, so all `size_of::<T>()` bytes
        // behind `record` are initialized, and a `u8` slice needs no alignment.
        let bytes = unsafe {
            std::slice::from_raw_parts(std::ptr::from_ref(record).cast::<u8>(), size_of::<T>())
        };
        self.bytes(bytes);
    }

    /// Appends `len` bytes of nothing. Whole regions of the preamble are zero,
    /// and a reserved field is written as zeros until it is patched.
    pub fn zeros(&mut self, len: usize) {
        self.data.resize(self.data.len().saturating_add(len), 0);
    }

    /// Pads with zeros until the next `to` boundary, and does nothing if that
    /// is where the buffer already ends.
    pub fn align(&mut self, to: usize) {
        let len = self.data.len();
        self.zeros(len.next_multiple_of(to).saturating_sub(len));
    }

    pub fn u8_at(&mut self, pos: usize, value: u8) {
        self.patch(pos, &[value]);
    }

    pub fn u16_at(&mut self, pos: usize, value: u16) {
        self.patch(pos, &value.to_be_bytes());
    }

    pub fn u32_at(&mut self, pos: usize, value: u32) {
        self.patch(pos, &value.to_be_bytes());
    }

    /// Overwrites a run of bytes at an absolute position: a field with no
    /// natural integer width, like a raw param blob, in a record that was
    /// reserved with [`zeros`](Self::zeros).
    pub fn bytes_at(&mut self, pos: usize, bytes: &[u8]) {
        self.patch(pos, bytes);
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.data
    }

    /// Overwrites bytes that were already written.
    ///
    /// Panics on a position the buffer has not reached, which cannot happen to
    /// a caller patching a field it reserved itself.
    #[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
    fn patch(&mut self, pos: usize, bytes: &[u8]) {
        self.data[pos..pos + bytes.len()].copy_from_slice(bytes);
    }
}

/// Resumes a buffer that already has bytes in it, so an already-finished
/// one can still be patched through `u8_at`/`u16_at`/`u32_at` instead of a
/// second, separate way of overwriting a position.
impl From<Vec<u8>> for Writer {
    fn from(data: Vec<u8>) -> Self {
        Self { data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reader

    #[test]
    fn reads_advance_and_stay_in_bounds() {
        let mut reader = Reader::new(&[0x00, 0x01, 0x02, 0x03, 0x04]);
        assert_eq!(reader.u32().unwrap(), 0x0001_0203);
        assert_eq!(reader.pos(), 4);
        assert_eq!(reader.u8().unwrap(), 0x04);
        assert!(matches!(reader.u8(), Err(ByteError::OutOfBounds { .. })));
    }

    #[test]
    fn absolute_reads_leave_the_cursor_alone() {
        let reader = Reader::new(&[0x0D, 0xEF, 0xAC, 0xED]);
        assert_eq!(reader.u16_at(2).unwrap(), 0xACED);
        assert_eq!(reader.pos(), 0);
    }

    /// A length that overflows when added to the position has to read as out of
    /// bounds rather than wrapping around into a range that happens to exist.
    #[test]
    fn absurd_lengths_do_not_wrap() {
        let reader = Reader::new(&[0u8; 8]);
        assert!(matches!(
            reader.slice_at(4, usize::MAX),
            Err(ByteError::OutOfBounds { .. })
        ));
    }

    layout! {
        struct Record {
            tag: u8,
            wide: Be32,
            narrow: Be16,
        }
    }

    /// Starting at an odd position proves the view never needed alignment.
    #[test]
    fn views_decode_big_endian_at_any_offset() {
        let reader = Reader::new(&[0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED]);
        let record: &Record = reader.view_at(1).unwrap();
        assert_eq!(record.tag, 0x0D);
        assert_eq!(record.wide.get(), 0x0001_0203);
        assert_eq!(record.narrow.get(), 0xACED);
        assert!(matches!(
            reader.view_at::<Record>(2),
            Err(ByteError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn tables_borrow_every_record_in_place() {
        let reader = Reader::new(&[
            0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED, 0x0E, 0, 0, 0, 1, 0, 2,
        ]);
        let records: &[Record] = reader.slice_of(1, 2).unwrap();
        assert_eq!(records[0].wide.get(), 0x0001_0203);
        assert_eq!(records[1].tag, 0x0E);
        assert_eq!(records[1].narrow.get(), 2);
        assert!(reader.slice_of::<Record>(2, 2).is_err());
        assert!(reader.slice_of::<Record>(0, usize::MAX).is_err());
    }

    #[test]
    fn a_written_record_reads_back() {
        let mut writer = Writer::new();
        writer.u8(0xFF);
        writer.record(&Record {
            tag: 0x0D,
            wide: Be32::new(0x0001_0203),
            narrow: Be16::new(0xACED),
        });
        assert_eq!(
            writer.finish(),
            [0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED]
        );
    }

    #[test]
    fn strings_stop_at_the_terminator() {
        let reader = Reader::new(b"name\0next\0");
        assert_eq!(reader.cstr_at(0).unwrap(), b"name");
        assert_eq!(reader.cstr_at(5).unwrap(), b"next");
        assert!(matches!(
            Reader::new(b"unterminated").cstr_at(0),
            Err(ByteError::Unterminated { .. })
        ));
    }

    // Writer

    #[test]
    fn writes_go_out_big_endian() {
        let mut writer = Writer::new();
        writer.u8(0x0D);
        writer.u16(0xACED);
        writer.u32(0x0001_0203);
        assert_eq!(writer.finish(), [0x0D, 0xAC, 0xED, 0x00, 0x01, 0x02, 0x03]);
    }

    /// The whole point of the writer: a field written before anybody knew what
    /// went in it, filled in once the rest had been laid down. Read back through
    /// the reader, since the two halves agreeing is what actually matters.
    #[test]
    fn a_reserved_field_is_filled_in_afterwards() {
        let mut writer = Writer::new();
        let field = writer.len();
        writer.u32(0);
        writer.bytes(b"name\0");

        let end = writer.len();
        writer.u32_at(field, u32::try_from(end).unwrap());

        let out = writer.finish();
        assert_eq!(Reader::new(&out).u32_at(field).unwrap(), 9);
    }

    #[test]
    fn padding_stops_on_the_next_boundary() {
        let mut writer = Writer::new();
        writer.bytes(b"abc");
        writer.align(4);
        assert_eq!(writer.len(), 4);

        // Already on one, so there is nothing to add.
        writer.align(4);
        assert_eq!(writer.len(), 4);
        assert_eq!(writer.finish(), *b"abc\0");
    }

    #[test]
    #[should_panic(expected = "range end index")]
    fn patching_past_the_end_is_refused() {
        let mut writer = Writer::new();
        writer.u32(0);
        writer.u32_at(2, 1);
    }
}
