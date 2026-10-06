//! What the game's tables say about one message file.
//!
//! That is which layout names its record fields, which field repeats the MID1
//! id, and what its text tags are called. Both editing and patching read a
//! file through [`Tables`].

pub mod text;

use std::ops::Range;

use tpmt_message::{Bmg, Encoding, TEXT_OFFSET_LEN, TextSegment};
use tpmt_tables::Edition;
use tpmt_tables::message::{self, Field, Layout};

use text::TextDiagnostic;

/// One message file as the tables read it. No edit changes the record
/// width, the MID1 or the encoding, so these hold for the file's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tables {
    /// The version and language the file is from, which pick its layouts and
    /// tag names.
    pub edition: Edition,
    pub encoding: Encoding,
    /// `None` when the edition reads no records this wide. See
    /// [`message::layout`].
    pub layout: Option<&'static Layout>,
    /// The field repeating [`Message::public_id`](tpmt_message::Message),
    /// which only a layout with an id has, and only in a file with a MID1.
    pub id: Option<Field>,
    pub has_mid1: bool,
    /// How many bytes each message's attributes hold.
    pub attributes_len: usize,
}

impl Tables {
    #[must_use]
    pub fn new(bmg: &Bmg, edition: Edition) -> Self {
        let layout = message::layout(edition, bmg.record_len);
        Self {
            edition,
            encoding: bmg.encoding,
            layout,
            id: bmg.mid1.and_then(|_| layout?.id),
            has_mid1: bmg.mid1.is_some(),
            attributes_len: usize::from(bmg.record_len.saturating_sub(TEXT_OFFSET_LEN)),
        }
    }

    /// The layout's field called `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&'static Field> {
        self.layout?.fields.iter().find(|field| field.name == name)
    }

    /// The layout's fields, the id aside, since `public_id` sets that.
    pub fn fields(&self) -> impl Iterator<Item = &'static Field> + '_ {
        let fields = self.layout.map_or(&[][..], |layout| layout.fields);
        fields.iter().filter(|field| !self.is_id(field))
    }

    /// Whether `field` covers any byte of the id.
    #[must_use]
    pub fn is_id(&self, field: &Field) -> bool {
        let id = self.id.as_ref().and_then(field_bytes);
        id.zip(field_bytes(field))
            .is_some_and(|(id, bytes)| bytes.start < id.end && id.start < bytes.end)
    }

    /// Whether the layout's fields describe `attributes` whole.
    #[must_use]
    pub const fn named(&self, attributes: &[u8]) -> bool {
        self.layout.is_some() && attributes.len() == self.attributes_len
    }

    /// Copies `public_id` into `attributes`, where they hold the id.
    pub fn set_id(&self, attributes: &mut [u8], public_id: u16) {
        let id = self.id.as_ref().and_then(field_bytes);
        if let Some(id) = id.and_then(|bytes| attributes.get_mut(bytes)) {
            id.copy_from_slice(&public_id.to_be_bytes());
        }
    }

    /// `text` as a string of text and named tags. See [`text::render`].
    #[must_use]
    pub fn render(&self, text: &[TextSegment]) -> String {
        text::render(text, self.encoding, self.edition)
    }

    /// A string from [`render`](Self::render) back into segments.
    ///
    /// # Errors
    ///
    /// As [`text::parse`].
    pub fn parse(&self, text: &str) -> Result<Vec<TextSegment>, Vec<TextDiagnostic>> {
        text::parse(text, self.encoding, self.edition)
    }
}

/// Where a record field sits in a message's attributes, or `None` when it
/// sits in the text offset.
#[must_use]
pub fn field_bytes(field: &Field) -> Option<Range<usize>> {
    let at = field.offset.checked_sub(usize::from(TEXT_OFFSET_LEN))?;
    Some(at..at + field.len)
}

/// A record field's value out of a message's attributes, or `None` when the
/// attributes are too short to hold it.
#[must_use]
pub fn read_field(attributes: &[u8], field: &Field) -> Option<u16> {
    match attributes.get(field_bytes(field)?)? {
        [byte] => Some(u16::from(*byte)),
        [high, low] => Some(u16::from_be_bytes([*high, *low])),
        _ => None,
    }
}

/// Writes a record field's value into a message's attributes. `None` when the
/// attributes are too short, or `value` doesn't fit a 1-byte field.
///
/// In a file with a MID1, the message id field repeats
/// [`Message::public_id`](tpmt_message::Message), so set that instead.
#[must_use]
pub fn write_field(attributes: &mut [u8], field: &Field, value: u16) -> Option<()> {
    match attributes.get_mut(field_bytes(field)?)? {
        [byte] => *byte = u8::try_from(value).ok()?,
        [high, low] => [*high, *low] = value.to_be_bytes(),
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use tpmt_message::Mid1Header;
    use tpmt_tables::Version;
    use tpmt_tables::message::{record, unit};

    use super::*;

    const EDITION: Edition = Edition::default_language(Version::GcnUsa);

    fn bmg(record_len: u16, mid1: bool) -> Bmg {
        Bmg {
            encoding: Encoding::ShiftJis,
            record_len,
            mid1: mid1.then(Mid1Header::default),
            messages: Vec::new(),
            flow: None,
            strings: None,
        }
    }

    fn field(offset: usize) -> Field {
        *record::LAYOUT
            .fields
            .iter()
            .find(|field| field.offset == offset)
            .unwrap()
    }

    #[test]
    fn fields_read_and_write_big_endian() {
        let label = field(0x06);
        let box_kind = field(0x09);
        let mut attributes = vec![0; 16];

        write_field(&mut attributes, &label, 0x1234).unwrap();
        write_field(&mut attributes, &box_kind, 13).unwrap();
        assert_eq!(&attributes[2..6], &[0x12, 0x34, 0, 13]);
        assert_eq!(read_field(&attributes, &box_kind), Some(13));
        assert_eq!(write_field(&mut attributes, &box_kind, 256), None);
    }

    #[test]
    fn the_id_is_a_field_only_with_a_mid1() {
        let tables = Tables::new(&bmg(record::LAYOUT.len, true), EDITION);
        assert!(tables.is_id(&record::ID));
        assert!(tables.fields().all(|field| *field != record::ID));

        let tables = Tables::new(&bmg(record::LAYOUT.len, false), EDITION);
        assert!(!tables.is_id(&record::ID));
    }

    /// A unit record's first field sits where the story record keeps its id,
    /// and a unit layout has no id, so it is never the id.
    #[test]
    fn a_unit_field_is_not_the_id() {
        let tables = Tables::new(&bmg(unit::LAYOUT.len, true), EDITION);
        assert!(!tables.is_id(&unit::LAYOUT.fields[0]));
    }
}
