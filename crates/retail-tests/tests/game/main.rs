//! Every claim `tpmt-game`'s tables make about retail files must hold for
//! every retail file. A value a table doesn't name is not a claim, so it
//! passes.
//!
//! Only claims a file can confirm are checked. Names, notes, and what the game
//! does with a value need the game running.
//!
//! Modules mirror `tpmt-game`'s, so each table's checks sit at the same path.

mod jsystem;

use std::process::ExitCode;

use tpmt_pipeline::FileKind;
use tpmt_retail_tests::Checks;
use tpmt_retail_tests::Source::Unpack;

use jsystem::jmessage;

/// One row per claim, so a failing trial names the claim.
#[rustfmt::skip]
static CHECKS: Checks = &[
    ("jsystem::jmessage::layout",  Unpack(FileKind::Mesg), jmessage::layout),
    ("jsystem::jmessage::id",      Unpack(FileKind::Mesg), jmessage::id),
    ("jsystem::jmessage::padding", Unpack(FileKind::Mesg), jmessage::padding),
    ("jsystem::jmessage::tags",    Unpack(FileKind::Mesg), jmessage::tags),
];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}
