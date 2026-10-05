//! Every retail file of a format we encode, loose or nested, must re-encode
//! to the bytes it was decoded from.
//!
//! Yaz0 re-encodes with `Strategy::Parity` (the vanilla game's search). Other
//! strategies aren't checked here.
//!
//! An archive round trip encodes the ids it decoded, so it stays exact once a
//! build links ids itself. That linking happens in packing before an
//! archive is encoded, so a test of it should compare where references land,
//! not bytes.

use std::process::ExitCode;

use tpmt_archive::Archive;
use tpmt_compression::Yaz0;
use tpmt_message::Bmg;
use tpmt_retail_tests::{Checks, round_trip};

/// A new format is one more row.
static CHECKS: Checks = &[
    round_trip!("archive::rarc", Archive),
    round_trip!("compression::yaz0", Yaz0),
    round_trip!("message::bmg", Bmg),
];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}
