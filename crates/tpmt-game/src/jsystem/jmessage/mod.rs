//! Message files: what the bytes inside a BMG mean to the game.

pub mod flow;
pub mod record;
pub mod tag;
pub mod unit;

use crate::{Entry, entry};

/// One field of an INF1 record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// Byte offset into the whole record, where 0x00 is the text offset.
    pub offset: usize,
    /// 1 or 2 bytes, big-endian.
    pub len: usize,
    pub name: &'static str,
    pub notes: &'static str,
    /// The table naming the field's values, for a 1-byte field that has one.
    pub values: Option<&'static [Entry<u8>]>,
    /// The value is a byte offset into STR1, and the string there is what the
    /// game uses.
    pub string: bool,
}

const fn field(offset: usize, len: usize, name: &'static str) -> Field {
    Field {
        offset,
        len,
        name,
        notes: "",
        values: None,
        string: false,
    }
}

impl Field {
    const fn notes(self, notes: &'static str) -> Self {
        Self { notes, ..self }
    }

    const fn values(self, values: &'static [Entry<u8>]) -> Self {
        Self {
            values: Some(values),
            ..self
        }
    }

    const fn string(self) -> Self {
        Self {
            string: true,
            ..self
        }
    }
}

/// A record struct the game reads INF1 with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Record width, text offset included.
    pub len: u16,
    /// Every field after the 4-byte text offset, in record order.
    pub fields: &'static [Field],
    /// The field that repeats the MID1 id, in a layout that has one.
    pub id: Option<Field>,
}

/// Every layout the game reads. The game reads each file with a fixed struct
/// and no two share a width, so the width picks the layout.
pub static LAYOUTS: &[Layout] = &[record::LAYOUT, unit::LAYOUT, unit::JPN_LAYOUT];

/// The layout `record_len` wide, or `None` when the game reads none that wide.
#[must_use]
pub fn layout(record_len: u16) -> Option<&'static Layout> {
    LAYOUTS.iter().find(|layout| layout.len == record_len)
}

/// Rows of `(value, name, rgb)`, where `None` is the box's own default.
/// [`COLORS`] and [`RGB`] both come from here, so they can't disagree.
#[rustfmt::skip]
const PALETTE: [(u8, &str, Option<[u8; 3]>); 9] = [
    (0, "Default", None),
    (1, "Red",     Some([0xF0, 0x78, 0x78])),
    (2, "Green",   Some([0xAA, 0xDC, 0x8C])),
    (3, "Blue",    Some([0xA0, 0xB4, 0xDC])),
    (4, "Yellow",  Some([0xDC, 0xDC, 0x82])),
    (5, "Sky",     Some([0xB4, 0xC8, 0xE6])),
    (6, "Purple",  Some([0xC8, 0xA0, 0xDC])),
    (7, "White",   Some([0xFF, 0xFF, 0xFF])),
    (8, "Orange",  Some([0xDC, 0xAA, 0x78])),
];

/// The palette the color tag indexes. Retail draws any index past the end as
/// white.
pub static COLORS: &[Entry<u8>] = &colors(PALETTE);

/// Each color's RGB, indexed by its value. `None` for the box's own default.
pub static RGB: &[Option<[u8; 3]>] = &rgb(PALETTE);

const fn colors<const N: usize>(rows: [(u8, &'static str, Option<[u8; 3]>); N]) -> [Entry<u8>; N] {
    let mut out = [entry(0, ""); N];
    let mut at = 0;
    while at < N {
        let (value, name, _) = rows[at];
        out[at] = entry(value, name);
        at += 1;
    }
    out
}

const fn rgb<const N: usize>(rows: [(u8, &str, Option<[u8; 3]>); N]) -> [Option<[u8; 3]>; N] {
    let mut out = [None; N];
    let mut at = 0;
    while at < N {
        out[at] = rows[at].2;
        at += 1;
    }
    out
}

/// The languages a PAL disc ships one BMG set for. 5 (Dutch) is unused.
#[rustfmt::skip]
pub static LANGUAGES: &[Entry<u8>] = &[
    entry(0, "English"),
    entry(1, "German"),
    entry(2, "French"),
    entry(3, "Spanish"),
    entry(4, "Italian"),
    entry(6, "Japanese"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find;

    /// [`RGB`] is indexed by value, so the palette has to be dense from 0.
    #[test]
    fn the_palette_is_dense() {
        assert!((0..).zip(COLORS).all(|(value, color)| color.value == value));
    }

    /// Each layout's fields tile its record after the text offset, with no
    /// gap or overlap.
    #[test]
    fn the_fields_cover_the_record() {
        for layout in LAYOUTS {
            let end = layout.fields.iter().try_fold(4, |at, field| {
                (field.offset == at).then_some(at + field.len)
            });
            assert_eq!(end, Some(usize::from(layout.len)), "{layout:?}");
        }
    }

    /// [`layout`] picks by width alone.
    #[test]
    fn no_two_layouts_share_a_width() {
        for layout in LAYOUTS {
            assert_eq!(super::layout(layout.len), Some(layout));
        }
    }

    /// A layout's id is one of its fields, so it tiles with the rest.
    #[test]
    fn the_id_is_a_field() {
        for layout in LAYOUTS {
            assert!(layout.id.is_none_or(|id| layout.fields.contains(&id)));
        }
    }

    #[test]
    fn the_palette_ends_at_orange() {
        assert_eq!(find(COLORS, 8).map(|color| color.name), Some("Orange"));
        assert_eq!(find(COLORS, 9), None);
        assert_eq!(RGB.get(8), Some(&Some([0xDC, 0xAA, 0x78])));
    }
}
