//! Every retail file of a leaf format.
//!
//! Re-encode must match the bytes it was decoded from.
//! These test the formats alone, not what the game makes of them.

use std::process::ExitCode;

use tpmt_message::Bmg;
use tpmt_retail_tests::{Checks, round_trip};

/// A new leaf format is one more row.
static CHECKS: Checks = &[round_trip!("message::bmg", Bmg)];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}
