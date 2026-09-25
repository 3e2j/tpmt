//! Every retail file of a leaf format must re-encode to the same bytes it was
//! decoded from. These test the formats alone, not what the game makes of
//! them.
//!
//! Archives, Yaz0 streams and the disc layout aren't compared. A rebuild
//! regenerates their bytes, so a byte diff can't tell a regression from
//! expected churn, and they have their own tests.

use std::process::ExitCode;

use tpmt_format::Format;
use tpmt_jmessage::Bmg;
use tpmt_pipeline::FileKind;
use tpmt_tests::Checks;

/// A new leaf format is one more row.
static CHECKS: Checks = &[("bmg", FileKind::Bmg, round_trip::<Bmg>)];

fn main() -> ExitCode {
    tpmt_tests::run(CHECKS)
}

/// The one problem with `original`, if there is one.
fn round_trip<F: for<'a> Format<'a>>(_path: &str, original: &[u8]) -> Vec<String> {
    let result = (|| {
        if !F::recognises(original) {
            return Err("not recognised".to_owned());
        }
        let decoded = F::decode(original).map_err(|error| format!("decode failed: {error}"))?;
        // TODO: once a format holds cross-references, stand in for the linker
        // here. Hand each id the decode gave out straight back as its own
        // resolution, so the encode writes the retail id and this stays a test
        // of the format alone.
        let rebuilt = decoded
            .encode()
            .map_err(|error| format!("encode failed: {error}"))?;
        first_difference(original, &rebuilt).map_or(Ok(()), |at| {
            Err(format!(
                "differs at {at:#x} (0x{:x} bytes in, 0x{:x} out)",
                original.len(),
                rebuilt.len()
            ))
        })
    })();
    result.err().into_iter().collect()
}

/// The first offset the two disagree at, counting one running out early as a
/// disagreement.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(a, b)| a != b)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}
