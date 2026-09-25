//! What the tests against retail data share: each disc in `discs/`, unpacked
//! once by the real [`tpmt_pipeline::unpack`], and a way to run a check over
//! every file of one [`FileKind`] in it, found through the unpack's own
//! [`tpmt_pipeline::formats`] rather than by name.
//!
//! Each check gets one trial per `.iso` and `.ciso`, because each region
//! ships different files. With no discs, one ignored trial per check stands
//! in and a note says why, so a run without discs never looks like a pass.
//!
//! The unpacks go under the OS temp directory and are never deleted by the
//! tests, so they outlive a run and the next run only reads them back. A disc
//! is unpacked again when its image changes, when [`FileKind`] gains a kind,
//! or when the OS clears the directory. Delete `<disc>.stamp` in
//! `tpmt-retail-tests/` in the temp directory to force it after changing what
//! an unpack writes. See [`unpacked`].

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libtest_mimic::{Arguments, Failed, Trial};
use rayon::prelude::*;
use tpmt_pipeline::{FileKind, Progress};

/// Reports past this many are counted but not printed.
const REPORT_LIMIT: usize = 50;

/// Checks one file, given its path in the unpack and its bytes, and returns
/// its problems, empty for a pass.
pub type Check = fn(&str, &[u8]) -> Vec<String>;

/// One check: its trial name, the kind of file it reads, and the check itself.
pub type Checks = &'static [(&'static str, FileKind, Check)];

/// A test binary's whole `main`.
#[must_use]
pub fn run(checks: Checks) -> ExitCode {
    let args = Arguments::from_args();
    let trials = match discs() {
        Ok(discs) if discs.is_empty() => {
            if !args.list {
                eprintln!(
                    "note: no .iso or .ciso in `{}`, so the tests against retail data are skipped",
                    discs_dir().display()
                );
            }
            checks
                .iter()
                .map(|(name, _, _)| Trial::test(*name, || Ok(())).with_ignored_flag(true))
                .collect()
        }
        Ok(discs) => discs
            .iter()
            .flat_map(|iso| checks.iter().map(|check| trial(*check, iso)))
            .collect(),
        Err(error) => vec![Trial::test("discs", move || Err(error.into()))],
    };
    libtest_mimic::run(&args, trials).exit_code()
}

fn discs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../discs")
}

/// Every disc image in `discs/`, sorted. Empty if the directory is missing.
fn discs() -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(discs_dir()) {
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

/// One check on one disc. Named `<check>::<image>` so a filter can pick out a
/// check, a region, or both.
fn trial((name, kind, check): (&str, FileKind, Check), iso: &Path) -> Trial {
    let name = format!("{name}::{}", file_name(iso));
    let iso = iso.to_path_buf();
    Trial::test(name, move || {
        let project = unpacked(&iso)?;
        let paths = tpmt_pipeline::formats(&project)?
            .remove(&kind)
            .unwrap_or_default();
        report(&each(&tpmt_pipeline::base(&project), &paths, check)?)
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// What one check came to on one disc.
struct Tally {
    /// Files the check read, passed or not.
    checked: usize,
    /// One line each, led by the file's path in the unpack, sorted.
    failures: Vec<String>,
}

fn report(Tally { checked, failures }: &Tally) -> Result<(), Failed> {
    if !failures.is_empty() {
        let mut report = format!("{} problems in {checked} files", failures.len());
        for failure in failures.iter().take(REPORT_LIMIT) {
            report.push_str("\n  ");
            report.push_str(failure);
        }
        return Err(report.into());
    }
    if *checked == 0 {
        return Err("no file on the disc was checked".into());
    }
    println!("{checked} files checked");
    Ok(())
}

/// The project `iso` unpacks to, unpacking it first unless the last unpack
/// was of the same image.
///
/// A stamp file beside the project records the image it came from and every
/// [`FileKind`], since a kind added later needs an unpack to be listed. It stays
/// locked from the read to the write, so a disc's trials, started together
/// in their own processes, unpack it once between them. The stamp is written
/// only after the unpack finishes, so one that fails part way is redone.
fn unpacked(iso: &Path) -> Result<PathBuf, Failed> {
    let root = std::env::temp_dir().join("tpmt-retail-tests");
    fs::create_dir_all(&root)?;
    let name = file_name(iso);
    let project = root.join(&name);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(format!("{name}.stamp")))?;
    file.lock()?;

    let disc = fs::metadata(iso)?;
    let mut stamp = format!("{} {:?}", disc.len(), disc.modified()?);
    for kind in FileKind::ALL {
        stamp.push(' ');
        stamp.push_str(kind.name());
    }
    let mut saved = String::new();
    file.read_to_string(&mut saved)?;
    if saved == stamp && tpmt_pipeline::is_project(&project) {
        return Ok(project);
    }

    file.set_len(0)?;
    tpmt_pipeline::unpack(iso, &project, &Progress::default())?;
    file.rewind()?;
    file.write_all(stamp.as_bytes())?;
    Ok(project)
}

/// Every one of `paths` under `base` through `check`.
fn each(base: &Path, paths: &BTreeSet<String>, check: Check) -> io::Result<Tally> {
    let failures = paths
        .par_iter()
        .map(|path| {
            let mut bytes = Vec::new();
            File::open(base.join(path))?.read_to_end(&mut bytes)?;
            let problems = check(path, &bytes).into_iter();
            Ok(problems
                .map(|problem| format!("`{path}`: {problem}"))
                .collect())
        })
        .collect::<io::Result<Vec<Vec<String>>>>()?;
    let mut failures = failures.concat();
    failures.sort();
    Ok(Tally {
        checked: paths.len(),
        failures,
    })
}
