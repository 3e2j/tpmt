use crate::Record;

/// A buffer being built up, append only.
///
/// Nothing here is fallible. The buffer grows to fit whatever is appended.
/// A value only known once later records are laid down belongs in a record
/// kept aside until it is, rather than patched in here afterwards.
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
    /// lands at.
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
    pub fn record<T: Record>(&mut self, record: &T) {
        self.bytes(record.as_bytes());
    }

    /// Appends `len` bytes of nothing.
    pub fn zeros(&mut self, len: usize) {
        self.data.resize(self.data.len().saturating_add(len), 0);
    }

    /// Pads with zeros until the next `to` boundary, and does nothing if that
    /// is where the buffer already ends.
    pub fn align(&mut self, to: usize) {
        let len = self.data.len();
        self.zeros(len.next_multiple_of(to).saturating_sub(len));
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Sample;
    use crate::{Be16, Be32};

    #[test]
    fn writes_go_out_big_endian() {
        let mut writer = Writer::new();
        writer.u8(0x0D);
        writer.u16(0xACED);
        writer.u32(0x0001_0203);
        assert_eq!(writer.finish(), [0x0D, 0xAC, 0xED, 0x00, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn a_written_record_reads_back() {
        let mut writer = Writer::new();
        writer.u8(0xFF);
        writer.record(&Sample {
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
}
