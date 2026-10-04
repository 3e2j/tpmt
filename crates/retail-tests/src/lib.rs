//! What the tests against retail data share.
//!
//! A retail test checks our code against what the disc holds. A test whose
//! expected value comes from our own code belongs in that code's crate.
//!
//! A [`Check::File`] sees every file of its kind at any depth, one at a time.
//!
//! A [`Check::Disc`] sees the whole disc at once, as a project unpacked from
//! it, for what spans files.
//!
//! Each check gets one trial per `.iso` and `.ciso`, because each region
//! ships different files. With no discs, one ignored trial per check stands
//! in and a note says why, so a run without discs never looks like a pass.
//!
//! Checks read an unpack of each disc, cached in the OS temp directory and
//! kept between runs for faster test results. A check on a packaging kind
//! (see [`FileKind::is_packaging`]) walks the image each run instead, since
//! an unpack doesn't keep packaging.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use libtest_mimic::{Arguments, Failed, Trial};
use rayon::prelude::*;
use tpmt_disc::{Boot, Disc};
use tpmt_project::{FileKind, Project};
use tpmt_report::Progress;
use tpmt_tables::Version;

/// Reports past this many are counted but not printed.
const REPORT_LIMIT: usize = 50;

/// One layer of a disc file, as the disc holds it.
pub struct File<'a> {
    /// What the game makes of a file can differ by release.
    pub version: Version,
    /// Its project path. A wrapper and the bytes inside it share one.
    pub path: &'a str,
    pub bytes: &'a [u8],
}

/// What a check reads. Each returns its problems, empty for a pass.
#[derive(Clone, Copy)]
pub enum Check {
    /// Every layer of this kind on the disc, one at a time.
    File(FileKind, fn(&File) -> Vec<String>),
    /// The whole disc, for what spans files.
    Disc(fn(Version, &Project) -> Vec<String>),
}

/// One check per row, under its trial name.
pub type Checks = &'static [(&'static str, Check)];

/// A row checking that every layer of a format encodes back to the bytes it
/// decoded from: `round_trip!("message::bmg", Bmg)`.
///
/// A macro, since a generic fn can't name a format that borrows from its
/// input, like `Archive<'a>`, at every lifetime.
#[macro_export]
macro_rules! round_trip {
    ($name:literal, $format:ident) => {
        (
            $name,
            $crate::Check::File(<$format as ::tpmt_binary::Format>::KIND, |file| {
                $crate::round_trip(file.bytes, |bytes| {
                    let decoded = <$format as ::tpmt_binary::Format>::decode(bytes)
                        .map_err(|error| format!("decode failed: {error}"))?;
                    // TODO: once a format holds cross-references, stand in for
                    // the linker here. Hand each id the decode gave out
                    // straight back as its own resolution, so the encode
                    // writes the retail id and this stays a test of the format
                    // alone.
                    ::tpmt_binary::Format::encode(&decoded)
                        .map_err(|error| format!("encode failed: {error}"))
                })
            }),
        )
    };
}

/// What [`round_trip!`] runs, with `encode` decoding and encoding the bytes
/// it's given.
#[doc(hidden)]
pub fn round_trip(
    original: &[u8],
    encode: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
) -> Vec<String> {
    match encode(original) {
        Ok(rebuilt) => differs(original, &rebuilt).into_iter().collect(),
        Err(problem) => vec![problem],
    }
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
                .map(|(name, _)| Trial::test(*name, || Ok(())).with_ignored_flag(true))
                .collect()
        }
        Ok(discs) => discs
            .iter()
            .flat_map(|iso| checks.iter().map(|&(name, check)| trial(name, check, iso)))
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
fn trial(name: &str, check: Check, iso: &Path) -> Trial {
    let name = format!("{name}::{}", file_name(iso));
    let iso = iso.to_path_buf();
    Trial::test(name, move || match check {
        Check::File(kind, check) if kind.is_packaging() => report(walked(&iso, kind, check)?),
        Check::File(kind, check) => report(stored(&iso, kind, check)?),
        Check::Disc(check) => whole(&iso, check),
    })
}

/// `check` over the unpack of `iso` as a whole.
fn whole(iso: &Path, check: fn(Version, &Project) -> Vec<String>) -> Result<(), Failed> {
    let project = unpacked(iso)?;
    let version = version(&project.boot()?)?;
    let mut problems = check(version, &project);
    problems.sort();
    failed(&problems, "on the disc").map_or(Ok(()), Err)
}

/// `check` over every file of `kind` in the unpack of `iso`.
fn stored(iso: &Path, kind: FileKind, check: fn(&File) -> Vec<String>) -> Result<Tally, Failed> {
    let project = unpacked(iso)?;
    let version = version(&project.boot()?)?;
    let base = project.base();
    let paths = project.formats()?.remove(&kind).unwrap_or_default();
    let tally = paths
        .par_iter()
        .map(|path| {
            let mut bytes = Vec::new();
            fs::File::open(base.join(path))?.read_to_end(&mut bytes)?;
            let mut tally = Tally::default();
            tally.check(
                check,
                &File {
                    version,
                    path,
                    bytes: &bytes,
                },
            );
            Ok(tally)
        })
        .try_reduce(Tally::default, |all, one| {
            Ok::<_, io::Error>(all.merge(one))
        })?;
    Ok(tally)
}

/// `check` over every layer of `kind` on the image `iso`, walked with
/// [`tpmt_pipeline::explode`], since the unpack never stores packaging.
///
/// Runs inside the walk's sink, so only one disc file's layers are held at a
/// time.
fn walked(iso: &Path, kind: FileKind, check: fn(&File) -> Vec<String>) -> Result<Tally, Failed> {
    let disc = Disc::open(iso)?;
    let version = version(&disc.metadata().boot)?;
    let entries = disc.entries()?;
    let tally = entries
        .par_iter()
        .filter_map(|entry| Some((entry.path(), entry.span()?)))
        .map(|(path, span)| {
            let data = disc.read(span)?;
            let mut tally = Tally::default();
            let walked = tpmt_pipeline::explode(
                path,
                &data,
                &mut |layer| {
                    if layer.kind == Some(kind) {
                        let file = File {
                            version,
                            path: layer.path,
                            bytes: layer.bytes,
                        };
                        tally.check(check, &file);
                    }
                    Ok(())
                },
                &mut Vec::new(),
            );
            if let Err(error) = walked {
                tally.failures.push(format!("walk failed at {error}"));
            }
            Ok(tally)
        })
        .try_reduce(Tally::default, |all, one| {
            Ok::<_, tpmt_disc::Error>(all.merge(one))
        })?;
    Ok(tally)
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
#[derive(Default)]
struct Tally {
    /// Layers the check read, passed or not.
    checked: usize,
    /// One line each, led by the layer's path.
    failures: Vec<String>,
}

impl Tally {
    fn check(&mut self, check: fn(&File) -> Vec<String>, file: &File) {
        let problems = check(file).into_iter();
        self.checked += 1;
        self.failures
            .extend(problems.map(|problem| format!("`{}`: {problem}", file.path)));
    }

    fn merge(mut self, other: Self) -> Self {
        self.checked += other.checked;
        self.failures.extend(other.failures);
        self
    }
}

fn report(
    Tally {
        checked,
        mut failures,
    }: Tally,
) -> Result<(), Failed> {
    failures.sort();
    if let Some(failed) = failed(&failures, &format!("in {checked} files")) {
        return Err(failed);
    }
    if checked == 0 {
        return Err("no file on the disc was checked".into());
    }
    println!("{checked} files checked");
    Ok(())
}

/// The first [`REPORT_LIMIT`] of `problems` under a count, or `None` for
/// none.
fn failed(problems: &[String], scope: &str) -> Option<Failed> {
    if problems.is_empty() {
        return None;
    }
    let mut report = format!("{} problems {scope}", problems.len());
    for problem in problems.iter().take(REPORT_LIMIT) {
        report.push_str("\n  ");
        report.push_str(problem);
    }
    Some(report.into())
}

/// The project `iso` unpacks to, unpacking it first unless the last unpack
/// was of the same image.
///
/// A stamp file beside the project records the image it came from and every
/// [`FileKind`], since a kind added later needs an unpack to be listed. It stays
/// locked from the read to the write, so a disc's trials, started together
/// in their own processes, unpack it once between them. The stamp is written
/// only after the unpack finishes, so one that fails part way is redone.
///
/// The project is redone when the image changes, when [`FileKind`] gains a
/// kind, or when the OS clears the temp directory. Delete `<disc>.stamp` in
/// `tpmt-retail-tests/` there to force it after changing what an unpack
/// writes.
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
    if saved == stamp && tpmt_project::is_project(&project) {
        return Ok(Project::discover(&project)?);
    }

    file.set_len(0)?;
    let project = tpmt_pipeline::unpack(iso, &project, &Progress::default())?;
    file.rewind()?;
    file.write_all(stamp.as_bytes())?;
    Ok(project)
}
