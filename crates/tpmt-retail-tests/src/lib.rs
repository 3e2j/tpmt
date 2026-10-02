//! What the tests against retail data share.
//!
//! A check runs over the files of each disc in `discs/`, read from its unpack
//! or straight off the image (see [`Source`]), and gets the disc's [`Version`],
//! for what the game makes of a file.
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

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libtest_mimic::{Arguments, Failed, Trial};
use rayon::prelude::*;
use tpmt_disc::{Boot, Disc};
use tpmt_game::Version;
use tpmt_pipeline::{FileKind, Progress, Project};

/// Reports past this many are counted but not printed.
const REPORT_LIMIT: usize = 50;

/// Checks one file, given the disc's version, the file's path and its bytes,
/// and returns its problems, empty for a pass.
pub type Check = fn(Version, &str, &[u8]) -> Vec<String>;

/// One check: its trial name, the files it reads, and the check itself.
pub type Checks = &'static [(&'static str, Source, Check)];

/// Where a check's files come from.
#[derive(Clone, Copy)]
pub enum Source {
    /// Every file of this kind in the unpack, by its path there. Found through
    /// [`tpmt_pipeline::Project::formats`], not by name.
    Unpack(FileKind),
    /// Every file as the image holds it, by its path on the disc, for what an
    /// unpack doesn't keep, like the bytes of a Yaz0 stream.
    Image,
}

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
fn trial((name, source, check): (&str, Source, Check), iso: &Path) -> Trial {
    let name = format!("{name}::{}", file_name(iso));
    let iso = iso.to_path_buf();
    Trial::test(name, move || {
        let tally = match source {
            Source::Unpack(kind) => from_unpack(&iso, kind, check)?,
            Source::Image => from_image(&iso, check)?,
        };
        report(&tally)
    })
}

/// `check` over every file of `kind` in the unpack of `iso`.
fn from_unpack(iso: &Path, kind: FileKind, check: Check) -> Result<Tally, Failed> {
    let project = unpacked(iso)?;
    let version = version(&project.boot()?)?;
    let base = project.base();
    let paths = project.formats()?.remove(&kind).unwrap_or_default();
    let files = paths.par_iter().map(|path| {
        let mut bytes = Vec::new();
        File::open(base.join(path))?.read_to_end(&mut bytes)?;
        Ok((path.as_str(), bytes))
    });
    Ok(each::<io::Error>(version, files, check)?)
}

/// `check` over every file on the image `iso`.
fn from_image(iso: &Path, check: Check) -> Result<Tally, Failed> {
    let disc = Disc::open(iso)?;
    let version = version(&disc.metadata().boot)?;
    let entries = disc.entries()?;
    let files = entries
        .par_iter()
        .filter_map(|entry| Some((entry.path(), entry.span()?)))
        .map(|(path, span)| Ok((path, disc.read(span)?)));
    Ok(each::<tpmt_disc::Error>(version, files, check)?)
}

fn version(boot: &Boot) -> Result<Version, Failed> {
    Version::from_disc(&boot.id, boot.revision).ok_or_else(|| {
        format!(
            "`{}` revision {} is no known version",
            boot.id, boot.revision
        )
        .into()
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Where `rebuilt` first strays from `original`, as a round trip reports
/// it, or `None` when they match.
///
/// `original` is cut down to the length of `rebuilt` when everything past
/// that is zeros, since retail padded some files past where their format
/// ends. `rebuilt` is never cut.
#[must_use]
pub fn differs(original: &[u8], rebuilt: &[u8]) -> Option<String> {
    let padded = original
        .strip_prefix(rebuilt)
        .is_some_and(|tail| tail.iter().all(|&byte| byte == 0));
    if padded {
        return None;
    }
    first_difference(original, rebuilt).map(|at| {
        format!(
            "differs at {at:#x} (0x{:x} bytes in, 0x{:x} out)",
            original.len(),
            rebuilt.len()
        )
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

/// What one check came to on one disc.
struct Tally {
    /// Files the check read, passed or not.
    checked: usize,
    /// One line each, led by the file's path, sorted.
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
fn unpacked(iso: &Path) -> Result<Project, Failed> {
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
        return Ok(Project::discover(&project)?);
    }

    file.set_len(0)?;
    let project = tpmt_pipeline::unpack(iso, &project, &Progress::default())?;
    file.rewind()?;
    file.write_all(stamp.as_bytes())?;
    Ok(project)
}

/// Every one of `files` through `check`.
fn each<'a, E: Send>(
    version: Version,
    files: impl ParallelIterator<Item = Result<(&'a str, Vec<u8>), E>>,
    check: Check,
) -> Result<Tally, E> {
    let failures = files
        .map(|file| {
            let (path, bytes) = file?;
            Ok(check(version, path, &bytes)
                .into_iter()
                .map(|problem| format!("`{path}`: {problem}"))
                .collect())
        })
        .collect::<Result<Vec<Vec<String>>, E>>()?;
    let checked = failures.len();
    let mut failures = failures.concat();
    failures.sort();
    Ok(Tally { checked, failures })
}
