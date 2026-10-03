//! Every Yaz0 stream on a retail disc, loose or inside an archive.
//!
//! Must re-encode to the bytes it was decoded from with `Strategy::Parity`
//! (the vanilla game's search). Other strategies aren't checked here.
//!
//! Read from the image, since an unpack keeps no stream.

use std::borrow::Cow;
use std::process::ExitCode;

use tpmt_archive::Archive;
use tpmt_binary::{FileKind, Format};
use tpmt_compression::Yaz0;
use tpmt_retail_tests::{Checks, Source, differs};
use tpmt_tables::Version;

static CHECKS: Checks = &[("compression::yaz0", Source::Image, round_trip)];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}

fn round_trip(_: Version, _path: &str, data: &[u8]) -> Vec<String> {
    walk(data)
}

/// Checks `data` if it's a stream, then any archive members inside it.
/// A member's problems are led by its path.
fn walk(data: &[u8]) -> Vec<String> {
    let mut problems = Vec::new();
    let bare = match FileKind::identify(data) {
        Some(FileKind::Yaz0) => match Yaz0::decode(data) {
            Ok(yaz0) => {
                problems.extend(stream(data, &yaz0));
                yaz0.data
            }
            Err(error) => return vec![format!("decode failed: {error}")],
        },
        _ => Cow::Borrowed(data),
    };

    // Archive members are checked too, for more coverage.
    if FileKind::identify(&bare) == Some(FileKind::Rarc) {
        match Archive::decode(&bare) {
            Ok(archive) => {
                for member in &archive.files {
                    let inner = walk(member.data).into_iter();
                    problems.extend(inner.map(|problem| format!("`{}`: {problem}", member.path)));
                }
            }
            Err(error) => problems.push(format!("archive decode failed: {error}")),
        }
    }
    problems
}

/// What keeps `yaz0` from encoding back to `original`, if anything.
fn stream(original: &[u8], yaz0: &Yaz0) -> Option<String> {
    match yaz0.encode() {
        Ok(rebuilt) => differs(original, &rebuilt),
        Err(error) => Some(format!("encode failed: {error}")),
    }
}
