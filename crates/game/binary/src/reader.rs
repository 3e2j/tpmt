use crate::{ByteError, Record, Result, bytes_at, record};

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
    pub fn bytes_at(&self, pos: usize, len: usize) -> Result<&'a [u8]> {
        bytes_at(self.data, pos, len)
    }

    /// Borrows `len` bytes at the cursor and steps over them.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if `len` bytes are not left in the
    /// buffer.
    pub fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let out = self.bytes_at(self.pos, len)?;
        // `bytes_at` above already proved `self.pos + len` fits in the buffer.
        self.pos = self.pos.saturating_add(len);
        Ok(out)
    }

    /// Reads a fixed-size array at the cursor and steps over it.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let out = self.array_at(self.pos)?;
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

    /// Copies `N` bytes at an absolute position into a fixed-size array.
    fn array_at<const N: usize>(&self, pos: usize) -> Result<[u8; N]> {
        let bytes = self.bytes_at(pos, N)?;
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
    pub fn record_at<T: Record>(&self, pos: usize) -> Result<&'a T> {
        record::record_at(self.data, pos)
    }

    /// Borrows `count` records laid end to end at an absolute position, the
    /// way a file stores a table of them.
    ///
    /// # Errors
    ///
    /// Returns [`ByteError::OutOfBounds`] if the table runs past the end of
    /// the buffer, or its length overflows.
    pub fn records_at<T: Record>(&self, pos: usize, count: usize) -> Result<&'a [T]> {
        record::records_at(self.data, pos, count)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Sample;

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
        assert_eq!(reader.bytes_at(2, 2).unwrap(), [0xAC, 0xED]);
        assert_eq!(reader.pos(), 0);
    }

    /// A length that overflows when added to the position has to read as out of
    /// bounds rather than wrapping around into a range that happens to exist.
    #[test]
    fn absurd_lengths_do_not_wrap() {
        let reader = Reader::new(&[0u8; 8]);
        assert!(matches!(
            reader.bytes_at(4, usize::MAX),
            Err(ByteError::OutOfBounds { .. })
        ));
    }

    /// Starting at an odd position proves the record never needed alignment.
    #[test]
    fn records_decode_big_endian_at_any_offset() {
        let reader = Reader::new(&[0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED]);
        let record: &Sample = reader.record_at(1).unwrap();
        assert_eq!(record.tag, 0x0D);
        assert_eq!(record.wide.get(), 0x0001_0203);
        assert_eq!(record.narrow.get(), 0xACED);
        assert!(matches!(
            reader.record_at::<Sample>(2),
            Err(ByteError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn tables_borrow_every_record_in_place() {
        let reader = Reader::new(&[
            0xFF, 0x0D, 0x00, 0x01, 0x02, 0x03, 0xAC, 0xED, 0x0E, 0, 0, 0, 1, 0, 2,
        ]);
        let records: &[Sample] = reader.records_at(1, 2).unwrap();
        assert_eq!(records[0].wide.get(), 0x0001_0203);
        assert_eq!(records[1].tag, 0x0E);
        assert_eq!(records[1].narrow.get(), 2);
        assert!(reader.records_at::<Sample>(2, 2).is_err());
        assert!(reader.records_at::<Sample>(0, usize::MAX).is_err());
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
}
