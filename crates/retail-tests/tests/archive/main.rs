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

use std::process::ExitCode;

use tpmt_archive::Archive;
use tpmt_binary::{Compression, Format};
use tpmt_pipeline::FileKind;
use tpmt_retail_tests::{Check, Checks, File, round_trip};

static CHECKS: Checks = &[
    ("archive::flags", Check::File(FileKind::Rarc, flags)),
    round_trip!("archive::round_trip", Archive),
];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}

/// Every member whose entry states a compression its bytes don't open with.
fn flags(file: &File) -> Vec<String> {
    let archive = match Archive::decode(file.bytes) {
        Ok(archive) => archive,
        Err(error) => return vec![format!("decode failed: {error}")],
    };
    let describe =
        |compression: Option<Compression>| compression.map_or("uncompressed", Compression::name);
    archive
        .files
        .iter()
        .filter_map(|member| {
            let found = Compression::of(member.data);
            (member.compression != found).then(|| {
                format!(
                    "`{}`: its entry says {} but its bytes are {}",
                    member.path,
                    describe(member.compression),
                    describe(found)
                )
            })
        })
        .collect()
}
