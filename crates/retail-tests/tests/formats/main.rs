//! Every retail file of a leaf format.
//!
//! Re-encode must match the bytes it was decoded from.
//! These test the formats alone, not what the game makes of them.
//!

use std::process::ExitCode;

use tpmt_format::Format;
use tpmt_game::Version;
use tpmt_jmessage::Bmg;
use tpmt_retail_tests::{Check, Checks, Source, differs};

/// A new leaf format is one more row.
static CHECKS: Checks = &[leaf::<Bmg>("jsystem::jmessage::bmg")];

fn main() -> ExitCode {
    tpmt_retail_tests::run(CHECKS)
}

/// A [`round_trip`] over every file of `F`'s kind in the unpack.
const fn leaf<F: for<'a> Format<'a>>(name: &'static str) -> (&'static str, Source, Check) {
    (name, Source::Unpack(F::KIND), round_trip::<F>)
}

/// The one problem with `original`, if there is one.
fn round_trip<F: for<'a> Format<'a>>(_: Version, _path: &str, original: &[u8]) -> Vec<String> {
    let result = (|| {
        F::KIND.check(original).map_err(|error| error.to_string())?;
        let decoded = F::decode(original).map_err(|error| format!("decode failed: {error}"))?;
        // TODO: once a format holds cross-references, stand in for the linker
        // here. Hand each id the decode gave out straight back as its own
        // resolution, so the encode writes the retail id and this stays a test
        // of the format alone.
        let rebuilt = decoded
            .encode()
            .map_err(|error| format!("encode failed: {error}"))?;
        differs(original, &rebuilt).map_or(Ok(()), Err)
    })();
    result.err().into_iter().collect()
}
