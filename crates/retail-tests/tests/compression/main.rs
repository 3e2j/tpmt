//! Every Yaz0 stream on a retail disc, loose or inside an archive.
//!
//! Must re-encode to the bytes it was decoded from with `Strategy::Parity`
//! (the vanilla game's search). Other strategies aren't checked here.

use std::process::ExitCode;

use tpmt_compression::Yaz0;
use tpmt_retail_tests::{Checks, round_trip};

static CHECKS: Checks = &[round_trip!("compression::yaz0", Yaz0)];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}
