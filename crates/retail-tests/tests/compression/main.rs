//! Every Yaz0 stream on a retail disc, loose or inside an archive.
//!
//! Must re-encode to the bytes it was decoded from with `Strategy::Parity`
//! (the vanilla game's search). Other strategies aren't checked here.
//!
//! Read from the image, since an unpack keeps no stream.

use std::process::ExitCode;

use tpmt_binary::Format;
use tpmt_compression::Yaz0;
use tpmt_retail_tests::{Checks, Source, differs, nested};
use tpmt_tables::Version;

static CHECKS: Checks = &[("compression::yaz0", Source::Image, round_trip)];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}

fn round_trip(_: Version, _path: &str, data: &[u8]) -> Vec<String> {
    nested(data, stream, |_, _| None)
}

/// What keeps `yaz0` from encoding back to `original`, if anything.
fn stream(original: &[u8], yaz0: &Yaz0) -> Option<String> {
    match yaz0.encode() {
        Ok(rebuilt) => differs(original, &rebuilt),
        Err(error) => Some(format!("encode failed: {error}")),
    }
}
