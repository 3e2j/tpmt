//! What the game's values mean, as static tables.
//!
//! Split by game subsystem, not by file format, because formats share values.
//! One item id can appear in several file formats. Every table is a
//! slice a picker can list whole, and every lookup returns `Option`, since a
//! mod can change the game's tables and an unknown value should show as its
//! raw number rather than fail.
//!
//! A table is written as one constructor call per row. The constructor takes
//! the columns every row has, and a method sets each column a row can leave
//! empty. Tables skip rustfmt so their columns stay aligned.
//!
//! Names and notes come from Dusklight and the decomp, they've not been manually
//! tested/verified in game.

// TODO: a golden test that holds every fact these tables state about retail
// data against every retail file on every disc. Today that is the record
// width in `bmg::record::LEN` and the argument length each `bmg::tag` row
// gives. A value a table doesn't name is not a claim, so the test
// shouldn't demand one. It belongs beside the round trip in
// `crates/formats/tpmt-tests`.

pub mod bmg;

/// One named value in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<T> {
    pub value: T,
    pub name: &'static str,
    /// Empty when the name says it all.
    pub notes: &'static str,
}

/// The entry for `value`, or `None` when the table doesn't name it.
#[must_use]
pub fn find<T: Copy + PartialEq>(
    table: &'static [Entry<T>],
    value: T,
) -> Option<&'static Entry<T>> {
    table.iter().find(|entry| entry.value == value)
}

const fn entry<T>(value: T, name: &'static str) -> Entry<T> {
    Entry {
        value,
        name,
        notes: "",
    }
}

impl<T: Copy> Entry<T> {
    const fn notes(self, notes: &'static str) -> Self {
        Self { notes, ..self }
    }
}
