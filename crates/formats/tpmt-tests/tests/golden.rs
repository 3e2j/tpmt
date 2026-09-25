//! Golden round trips. Every retail file that a leaf format recognises must
//! re-encode to the same bytes it was decoded from.
//!
//! Archives, Yaz0 streams and the disc layout aren't compared. A rebuild
//! regenerates their bytes, so a byte diff can't tell a regression from
//! expected churn, and they have their own tests. This test only unpacks them
//! through [`explode`], so it checks the same leaves an unpack writes out.
//!
//! Each `.iso` and `.ciso` in `discs/` gets its own trial because each region
//! ships different files. With no discs, a single ignored trial stands in and a
//! note says why, so a run without discs never looks like a pass.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libtest_mimic::{Arguments, Failed, Trial};
use rayon::prelude::*;
use tpmt_disc::Disc;
use tpmt_format::Format;
use tpmt_jmessage::Bmg;
use tpmt_pipeline::explode;

/// Reports past this many are counted but not printed.
const REPORT_LIMIT: usize = 50;

const NAME: &str = "round_trip";

fn main() -> ExitCode {
    let args = Arguments::from_args();
    let trials = match discs() {
        Ok(discs) if discs.is_empty() => {
            if !args.list {
                eprintln!(
                    "note: no .iso or .ciso in `{}`, so the golden round trips are skipped",
                    discs_dir().display()
                );
            }
            vec![Trial::test(NAME, || Ok(())).with_ignored_flag(true)]
        }
        Ok(discs) => discs.into_iter().map(trial).collect(),
        Err(error) => vec![Trial::test(NAME, move || Err(error.into()))],
    };
    libtest_mimic::run(&args, trials).exit_code()
}

fn discs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../discs")
}

/// Every disc image in `discs/`, sorted. Empty if the directory is missing.
fn discs() -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(discs_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut discs = entries
        .map(|entry| Ok(entry?.path()))
        .collect::<io::Result<Vec<_>>>()?;
    discs.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "iso" || extension == "ciso")
    });
    discs.sort();
    Ok(discs)
}

/// Named after the image, so a filter can pick out one region.
fn trial(iso: PathBuf) -> Trial {
    let file_name = iso.file_name().unwrap_or_default().to_string_lossy();
    let name = format!("{NAME}::{file_name}");
    Trial::test(name, move || check(&iso))
}

fn check(iso: &Path) -> Result<(), Failed> {
    let Tally { checked, failures } = check_disc(iso)?;
    if !failures.is_empty() {
        let mut report = format!("{} of {checked} failed", failures.len());
        for failure in failures.iter().take(REPORT_LIMIT) {
            report.push_str("\n  ");
            report.push_str(failure);
        }
        return Err(report.into());
    }
    if checked == 0 {
        return Err("no file on it was checked".into());
    }
    println!("{checked} files round tripped");
    Ok(())
}

/// What one disc, or one file of it, came to.
#[derive(Default)]
struct Tally {
    /// Files a format recognised and round tripped, passed or not.
    checked: usize,
    /// One line each, led by the innermost path through every archive the
    /// file sits in.
    failures: Vec<String>,
}

impl Tally {
    fn merge(mut self, other: Self) -> Self {
        self.checked += other.checked;
        self.failures.extend(other.failures);
        self
    }
}

/// Every file on one disc, with the failures sorted by path.
fn check_disc(iso: &Path) -> Result<Tally, tpmt_disc::Error> {
    let disc = Disc::open(iso)?;
    let mut tally = disc
        .entries()?
        .par_iter()
        .filter_map(|entry| Some((entry.path(), entry.span()?)))
        .map(|(path, span)| {
            let data = disc.read(span)?;
            let mut tally = Tally::default();
            let exploded = explode(path, &data, &mut |path, bare| {
                if let Some(result) = leaf(bare) {
                    tally.checked += 1;
                    if let Err(problem) = result {
                        tally.failures.push(format!("`{path}`: {problem}"));
                    }
                }
                Ok(())
            });
            // Already led by the innermost path, in the same style.
            if let Err(error) = exploded {
                tally.failures.push(error.to_string());
            }
            Ok::<_, tpmt_disc::Error>(tally)
        })
        .try_reduce(Tally::default, |a, b| Ok(a.merge(b)))?;
    tally.failures.sort();
    Ok(tally)
}

/// Round trips `bare` through whichever leaf format recognises it, or `None`
/// when none does. An archive's sidecar lands here too, and no format claims
/// it. A new leaf format is one more `.or_else`.
fn leaf(bare: &[u8]) -> Option<Result<(), String>> {
    round_trip::<Bmg>(bare)
}

/// `None` when `F` doesn't recognise `original`.
fn round_trip<F: for<'a> Format<'a>>(original: &[u8]) -> Option<Result<(), String>> {
    F::recognises(original).then(|| -> Result<(), String> {
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
    })
}

/// The first offset the two disagree at, counting one running out early as a
/// disagreement.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(a, b)| a != b)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}
