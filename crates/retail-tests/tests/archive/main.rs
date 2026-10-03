//! Every archive on a retail disc, loose or nested.
//!
//! What each entry's flags state about its member must agree with the
//! member's bytes, so a vanilla unpack raises no warning. Each archive must
//! also re-encode to the bytes it was decoded from, which covers every flag
//! bit the first check can't see.
//!
//! The round trip encodes the ids it decoded, so it stays exact once a build
//! links ids itself. That linking happens in the pipeline before an archive
//! is encoded, so a test of it should compare where references land, not
//! bytes.
//!
//! Read from the image, since an unpack keeps no flags.

use std::process::ExitCode;

use tpmt_archive::{Archive, Format};
use tpmt_retail_tests::{Checks, Source, differs, nested};
use tpmt_tables::Version;

static CHECKS: Checks = &[
    ("archive::flags", Source::Image, unwarned),
    ("archive::round_trip", Source::Image, round_trip),
];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}

/// Every warning exploding the file raises, or why it would not explode.
fn unwarned(_: Version, path: &str, data: &[u8]) -> Vec<String> {
    let mut reports = Vec::new();
    match tpmt_pipeline::explode(path, data, &mut |_| Ok(()), &mut reports) {
        Ok(_) => reports.iter().map(ToString::to_string).collect(),
        Err(error) => vec![format!("explode failed: {error}")],
    }
}

fn round_trip(_: Version, _path: &str, data: &[u8]) -> Vec<String> {
    nested(data, |_, _| None, rebuild)
}

/// What keeps `archive` from encoding back to `original`, if anything.
fn rebuild(original: &[u8], archive: &Archive) -> Option<String> {
    match archive.encode() {
        Ok(rebuilt) => differs(original, &rebuilt),
        Err(error) => Some(format!("archive encode failed: {error}")),
    }
}
