//! Every claim `tpmt-tables`'s tables make about retail files must hold for
//! every retail file. A value a table doesn't name is not a claim, so it
//! passes.
//!
//! Only claims a file can confirm are checked. Names, notes, and what the game
//! does with a value need the game running.
//!
//! Modules mirror `tpmt-tables`'s, so each table's checks sit at the same path.

mod message;

use std::process::ExitCode;

use tpmt_pipeline::FileKind;
use tpmt_retail_tests::Checks;
use tpmt_retail_tests::Source::Unpack;

/// One row per claim, so a failing trial names the claim.
#[rustfmt::skip]
static CHECKS: Checks = &[
    ("message::layout",  Unpack(FileKind::Mesg), message::layout),
    ("message::id",      Unpack(FileKind::Mesg), message::id),
    ("message::padding", Unpack(FileKind::Mesg), message::padding),
    ("message::tags",    Unpack(FileKind::Mesg), message::tags),
];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}
