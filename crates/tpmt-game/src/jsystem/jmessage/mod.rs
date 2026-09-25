//! Message files: what the bytes inside a BMG mean to the game.

pub mod flow;
pub mod record;
pub mod tag;

use crate::{Entry, entry};

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

    #[test]
    fn the_palette_ends_at_orange() {
        assert_eq!(find(COLORS, 8).map(|color| color.name), Some("Orange"));
        assert_eq!(find(COLORS, 9), None);
        assert_eq!(RGB.get(8), Some(&Some([0xDC, 0xAA, 0x78])));
    }
}
