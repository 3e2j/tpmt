//! Bounds-checked big-endian byte cursors (read/write).
//!
//! Everything on the disc is big-endian, and every format crate parses the same
//! shape: read a header, follow an offset into a table, read records at
//! computed positions. A reader does that over a borrowed buffer; a writer
//! builds one up and backpatches the offsets that were not knowable at the
//! point they were reserved.

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
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Moves the cursor. Landing past the end is not an error until something
    /// is actually read from there.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Borrows `len` bytes at an absolute position.
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
    pub fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let out = self.slice_at(self.pos, len)?;
        self.pos += len;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    // `at` positions don't silently advance cursor

    pub fn u8_at(&self, pos: usize) -> Result<u8> {
        Ok(self.slice_at(pos, 1)?[0])
    }

    pub fn u16_at(&self, pos: usize) -> Result<u16> {
        let bytes = self.slice_at(pos, 2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32_at(&self, pos: usize) -> Result<u32> {
        let bytes = self.slice_at(pos, 4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Borrows the null-terminated bytes at an absolute position, terminator
    /// excluded. What encoding they are in is the caller's business.
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
        Ok(&rest[..end])
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
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a buffer that will not have to grow on the way to `capacity`.
    /// Worth it for the file table, whose length is known before it is built.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
        }
    }

    /// How much has been written, which is also the position the next append
    /// lands at. Reserving a field to backpatch means keeping this first.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
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

    /// Appends `len` bytes of nothing. Whole regions of the preamble are zero,
    /// and a reserved field is written as zeros until it is patched.
    pub fn zeros(&mut self, len: usize) {
        self.data.resize(self.data.len() + len, 0);
    }

    /// Pads with zeros until the next `to` boundary, and does nothing if that
    /// is where the buffer already ends.
    pub fn align(&mut self, to: usize) {
        self.zeros(self.data.len().next_multiple_of(to) - self.data.len());
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

    pub fn finish(self) -> Vec<u8> {
        self.data
    }

    /// Overwrites bytes that were already written.
    ///
    /// Panics on a position the buffer has not reached, which cannot happen to
    /// a caller patching a field it reserved itself.
    fn patch(&mut self, pos: usize, bytes: &[u8]) {
        self.data[pos..pos + bytes.len()].copy_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        writer.u32_at(field, end as u32);

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
    #[should_panic]
    fn patching_past_the_end_is_refused() {
        let mut writer = Writer::new();
        writer.u32(0);
        writer.u32_at(2, 1);
    }
}
