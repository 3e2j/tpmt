//! What the game's values and layouts mean. Most of it is static tables of
//! named values. The rest is facts a table can't hold, like how many bytes a
//! value takes.
//!
//! Split by game subsystem, not by file format, because formats share values.
//!
//! Every table is a slice a picker can list whole, and every lookup returns
//! `Option`, since a mod can change the game's tables and an unknown value
//! should show as its raw number rather than fail.
//!
//! A table is written as one constructor call per row. The constructor takes
//! the columns every row has, and a method sets each column a row can leave
//! empty. Tables skip rustfmt so their columns stay aligned.
//!
//! Names and notes come from Dusklight and the decomp, they've not been manually
//! tested/verified in game.

// TODO: a golden test that holds every fact these tables state about retail
// data against every retail file on every disc. Today that is the record
// width of each layout `jsystem::jmessage::layouts` returns for the disc's
// version, and the argument length each `jsystem::jmessage::tag` row gives.
// A value a table doesn't name is not a claim, so the test shouldn't demand
// one. It belongs beside the round trip in `crates/tpmt-retail-tests`.

pub mod jsystem;
mod version;

pub use version::{Edition, Language, Version, Versions};

/// One named value in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<T> {
    pub value: T,
    pub name: &'static str,
    /// Empty when the name says it all.
    pub notes: &'static str,
    /// The versions this row holds for. A value whose meaning differs by
    /// version has one row per meaning, with disjoint sets.
    pub versions: Versions,
}

/// The entry for `value` on `edition`, or `None` when the table doesn't name
/// it there.
#[must_use]
pub fn find<T: Copy + PartialEq>(
    table: &'static [Entry<T>],
    value: T,
    edition: Edition,
) -> Option<&'static Entry<T>> {
    entries(table, edition).find(|entry| entry.value == value)
}

/// The rows of `table` that hold on `edition`, for a picker to list.
pub fn entries<T>(
    table: &'static [Entry<T>],
    edition: Edition,
) -> impl Iterator<Item = &'static Entry<T>> {
    table
        .iter()
        .filter(move |entry| entry.versions.contains(edition.version()))
}

const fn entry<T>(value: T, name: &'static str) -> Entry<T> {
    Entry {
        value,
        name,
        notes: "",
        versions: Versions::ALL,
    }
}

impl<T: Copy> Entry<T> {
    const fn notes(self, notes: &'static str) -> Self {
        Self { notes, ..self }
    }

    const fn only(self, versions: Versions) -> Self {
        Self { versions, ..self }
    }
}

/// Fails when two rows share a key on one version. `key` gives a row's key
/// and the versions it holds for.
#[cfg(test)]
fn assert_one_meaning<R, K: PartialEq + std::fmt::Debug>(
    rows: &[R],
    key: impl Fn(&R) -> (K, Versions),
) {
    let mut rest = rows;
    while let [row, tail @ ..] = rest {
        let (value, versions) = key(row);
        for other in tail {
            let (other_value, other_versions) = key(other);
            assert!(
                value != other_value || versions.is_disjoint(other_versions),
                "{value:?} has two meanings on one version",
            );
        }
        rest = tail;
    }
}
