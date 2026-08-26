//! Unpacking a disc to a project folder and building it back.
//!
//! Unpacking walks the disc, peels off compression, opens archives, hands
//! decodable files to whichever format crate claims them, and writes the
//! result out as a single folder a person edits in place. Building runs the
//! same route backwards.
//!
//! Building has two endings. A `build` works out what changed, re-encodes it,
//! and gathers the finished game files into a mod somebody else can install.
//! An `image` carries on from there and lays those files out as a disc, which
//! is the only step of the two that has to know where anything goes.
//! Everything before that split is one path, and neither ending is built by
//! way of the other.
//!
//! Edits are never tracked as they happen. Which files a person touched is
//! worked out at build time, by hashing them against the vanilla hashes taken
//! at unpack, and only those get re-encoded. Everything else is copied out of
//! the source disc verbatim as raw bytes.
//!
//! An archive is a container around its files, and a change to any of them is
//! a change to the whole thing: touched anywhere inside, the whole container
//! is repacked; untouched, it is copied off the disc whole, still compressed.
//!
//! Format crates convert their own file type to an editable form and back.
//! Which form that is belongs to the crate rather than here, so this one only
//! asks whether a conversion exists and takes back what it produces, extension
//! included. Their typed models are public as well, reachable by an editor
//! without a file in between.
//!
//! A format crate never touches a `Path` or `std::fs`: its functions take
//! bytes or its own types in, and hand bytes or its own types back. Reading
//! the input and writing whatever comes out of it happens here and nowhere
//! else. How many pieces a format hands back (an editable file on its own, or
//! one paired with a sidecar) is that format's own call, not a shape this
//! crate imposes on it.
//!
//! # Why
//!
//! **This is the conveyor belt: it takes the game apart into a folder of
//! editable files, and puts it back together into a disc. Every format the
//! game holds comes together here, and only here, to form the actual game.**
//!
//! - Assembling the game is a job of its own, separate from any format
//!   knowing how to decode itself. This crate is the mediator that does it:
//!   it walks the whole disc, hands each file to whichever format crate
//!   claims it, and folds the results back into one.
//! - Taking the game apart and putting it back together are the same route,
//!   walked in each direction: what unpack writes out is exactly what build
//!   folds back in, so nothing about assembling the game lives anywhere else.
//!
//! Paths mirror the disc, and decoded files chain extensions (zel_00.bmg.json).
//!
//! # External quirks to know
//!
//! - Some facts about a file do not survive being written out as one, like a
//!   wrapper that came off it or which memory an archive member loads into.
//!   Those go into a sidecar next to it instead.
//!
//! # Layout
//!
//! A project is one directory, edited in place:
//!
//! ```text
//! tpmt.toml   the preamble values a build cannot derive
//! sys/        apploader.img, main.dol
//! files/      game content, archives as directories
//! out/        built mods and images
//! ```
//!
//! Sidecars hold what a decoded file could not carry, one name per format:
//!
//! ```text
//! *.arc/.tpmt-arc.toml   what an unpacked archive is, minus its bytes
//! ```
//!
//! The store holds everything generated about the project rather than for it:
//!
//! ```text
//! .tpmt/source.toml   where the ISO was last seen, plus its sha1
//! .tpmt/hashes        vanilla hashes, for change detection
//! ```

// TODO: the golden roundtrip. Unpack a retail ISO, image it straight back
// with nothing edited, and diff the two.
//
// Byte equality is the wrong measurement for rebuilt entries. A format that
// stores something derived comes back derived rather than restored, so the
// bytes can move freely without anything being "wrong".

// TODO: routing tables (disc path to project path) are still hardcoded nowhere.
// Scope is Twilight Princess only, but GZ2E, GZ2P and GZ2J do not share paths,
// so whatever holds them is keyed by region.

// TODO: decide whether to keep our own copy of the ISO rather than remembering
// its originating path. Either way, tell the user before taking their disk space.

// TODO: a mod has no way to say a file was deleted, only which ones it
// replaces or adds. Only matters outside an archive, since a deleted member
// is already covered by the whole container repacking. An image handles a
// deletion fine either way, since it lays out whatever the tree holds. But
// a `build` folder can't.

// TODO: nothing here catches a deleted file that something else still
// references by id, path, or name; that only surfaces as a crash in game,
// far from the build that caused it. Two separate defenses belong here
// eventually: a build-time warning when a file the original archive held is
// gone from what gets packed (cheap, catches the common case, blind to
// whether anything actually referenced it), and, once a linker exists,
// rejecting a build outright when a reference resolves to nothing.

mod fs;
mod manifest;
mod plan;
mod revert;
mod sidecars;
mod store;
#[cfg(test)]
mod test_support;

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tpmt_disc::{BI2_PATH, BOOT_PATH, Disc, Entry, Layout, Metadata};

use crate::fs::Staged;
use crate::manifest::{FILES, Manifest, OUT, PROJECT_FILE, SYS};
use crate::plan::Output;
use crate::sidecars::arc::write_sidecar;
use crate::store::{Source, Store};

pub use crate::plan::{Change, ChangeKind};
pub use crate::revert::{RevertPlan, SidecarEntry};

/// The three Twilight Princess prints. Any other disc would unpack fine and
/// then mean nothing to the rest of the toolkit, so it is turned away here.
const SUPPORTED_IDS: [&str; 3] = ["GZ2E", "GZ2P", "GZ2J"];

/// One project leaf's vanilla hash, keyed by its project path.
pub(crate) struct FileHash {
    pub(crate) path: String,
    pub(crate) digest: String,
}

/// Finds the project root by walking upward from `start`, the same way git finds
/// `.git`: canonicalize first, then climb one directory at a time until a
/// `.tpmt` store turns up or the filesystem root is reached.
///
/// Checks only that `.tpmt` exists as a directory, not what is inside it,
/// since every project has had one since `unpack` first ran, whether or not
/// this build has started writing `.tpmt/tpmt.toml` yet.
pub fn discover(start: &Path) -> Result<PathBuf, Error> {
    let mut at = start.canonicalize().map_err(|source| Error::Read {
        path: start.to_path_buf(),
        source,
    })?;

    loop {
        if at.join(store::STORE).is_dir() {
            return Ok(at);
        }
        if !at.pop() {
            return Err(Error::NoProjectFound(start.to_path_buf()));
        }
    }
}

/// Whether `dir` is itself a project root, judged the same way `discover`
/// recognises one while walking upward: by the store directory every project
/// has and nothing else creates.
pub fn is_project(dir: &Path) -> bool {
    dir.join(store::STORE).is_dir()
}

/// Unpacks a disc image into a project directory, creating it if it does not
/// exist. An existing project is replaced only if `overwrite` is set;
/// anything else already there is refused.
pub fn unpack(iso: &Path, project: &Path, overwrite: bool) -> Result<(), Error> {
    // Checked before the disc is even opened: whether this directory is safe
    // to write into does not depend on what is in the ISO, so there is no
    // reason to make somebody wait on that just to be told no. An empty
    // directory holds nothing to protect or to reconcile, so it is left to
    // Staged below rather than judged here.
    if project.exists() && !fs::listing(project)?.is_empty() {
        if !is_project(project) {
            return Err(Error::ForeignDirectory(project.to_path_buf()));
        }
        if !overwrite {
            return Err(Error::ProjectExists(project.to_path_buf()));
        }
    }

    let disc = Disc::open(iso)?;
    let metadata = disc.metadata();
    let id = &metadata.boot.id;
    if !SUPPORTED_IDS.contains(&id.as_str()) {
        return Err(Error::UnsupportedDisc(id.clone()));
    }

    let staged = Staged::directory(project, &[PROJECT_FILE, FILES, SYS, OUT, store::STORE])?;
    let at = staged.path();
    Manifest::new(metadata).write(at)?;

    // One flat layer of work. Every entry reads its own bytes off the shared
    // disc and writes its own outputs, so there is nothing to hand between
    // threads and nothing to order them by.
    let hashes: Vec<FileHash> = disc
        .entries()?
        .par_iter()
        .map(|entry| unpack_disc_entry(&disc, entry, at))
        .collect::<Result<Vec<_>, Error>>()?
        .into_iter()
        .flatten()
        .collect();

    // Where the disc was, taken now rather than at build time: a project is
    // often built from somewhere else entirely.
    let found = iso.canonicalize().map_err(|source| Error::Read {
        path: iso.to_path_buf(),
        source,
    })?;
    let store = Store::new(at);
    store.write_source(&Source {
        path: found,
        sha1: disc.sha1()?,
    })?;
    store.write_hashes(hashes)?;

    staged.finish()?;
    Ok(())
}

/// Every project leaf that differs from vanilla.
///
/// Nothing here touches the source disc: a status only compares the
/// project's own files against the hashes taken at unpack, so it still works
/// with the disc missing or moved.
pub fn status(project: &Path) -> Result<Vec<Change>, Error> {
    Manifest::read(project)?;
    let vanilla = Store::new(project).hashes()?;
    plan::changes(project, &vanilla)
}

/// Works out what reverting `target` would do, without writing anything.
///
/// `target` is an absolute path to a leaf (put back on its own) or a
/// directory (every vanilla leaf under it put back together). Neither has to
/// exist on disk right now: a file already deleted from the project is as
/// revertable as one somebody edited.
pub fn revert_plan(project: &Path, target: &Path) -> Result<RevertPlan, Error> {
    let path = revert::resolve(project, target)?;
    revert::plan(project, &path)
}

/// Carries out a plan `revert_plan` already worked out.
///
/// `restore_sidecar_entry` only matters when the plan found one: it decides
/// whether the archive member being restored also gets its own entry in the
/// sidecar put back, without touching any other member's.
pub fn revert(project: &Path, plan: &RevertPlan, restore_sidecar_entry: bool) -> Result<(), Error> {
    revert::apply(project, plan, restore_sidecar_entry)
}

/// Packs the changes into a mod somebody else can install.
///
/// Nothing but game files, at the paths they go back to. Anything else in here
/// would be handed to whatever applies this as though it were a file the disc
/// holds, so a mod says what it has to say by what it contains and where.
pub fn build(project: &Path, out: Option<&Path>) -> Result<PathBuf, Error> {
    let (metadata, disc, vanilla) = open(project)?;
    let plan = plan::plan(project, &disc, &vanilla)?;

    let target = target(project, out, format!("{}-mod", print(&metadata)));
    let staged = Staged::directory(&target, &[SYS, FILES])?;

    for output in plan.outputs.iter().filter(|output| output.is_changed()) {
        fs::write(&staged.path().join(&output.path), &output.bytes(&disc)?)?;
    }
    preamble(&metadata, &disc, staged.path())?;

    staged.finish()
}

/// Writes out the two preamble pieces, and only if they came out different.
///
/// Neither is a file the project holds: they are kept as their values, so a
/// build is the first place their bytes exist to be compared. Both are built
/// over what the source disc had rather than from nothing, which leaves the
/// difference between them exactly the values somebody edited.
///
/// The boot header also records where the executable and the file table sit,
/// and those move on their own when either grows. They are not put in here:
/// they describe the disc this would have imaged, and a mod is installed into a
/// disc laid out by whatever installs it, which works them out again for the
/// image it is writing.
fn preamble(metadata: &Metadata, disc: &Disc, at: &std::path::Path) -> Result<(), Error> {
    let boot = disc.boot_bin()?;
    let bi2 = disc.bi2_bin()?;
    let pieces = [
        (
            BOOT_PATH,
            tpmt_disc::boot_bin_over(&boot, &metadata.boot)?,
            boot,
        ),
        (BI2_PATH, tpmt_disc::bi2_bin(&metadata.bi2), bi2),
    ];

    for (path, built, original) in pieces {
        if built != original {
            fs::write(&at.join(path), &built)?;
        }
    }
    Ok(())
}

/// Packs the changes into a playable disc image.
///
/// The same files a mod would hold, plus every one nobody touched, laid out as
/// a disc. Written straight through in one pass, since the layout hands them
/// over in the order they go on.
pub fn image(project: &Path, out: Option<&Path>) -> Result<PathBuf, Error> {
    let (metadata, disc, vanilla) = open(project)?;
    let plan = plan::plan(project, &disc, &vanilla)?;
    let layout = Layout::plan(&metadata, &plan.items)?;

    let target = target(project, out, format!("{}.iso", print(&metadata)));
    let staged = Staged::file(&target)?;
    let file = File::create(staged.path()).map_err(|source| Error::Write {
        path: staged.path().to_path_buf(),
        source,
    })?;

    let outputs: std::collections::HashMap<&str, &Output> = plan
        .outputs
        .iter()
        .map(|output| (output.path.as_str(), output))
        .collect();

    let mut image = layout.write(BufWriter::new(file));
    for entry in layout.entries() {
        let Entry::File { path, .. } = entry else {
            continue;
        };
        let output = outputs
            .get(path.as_str())
            .ok_or_else(|| Error::NotOnDisc(path.clone()))?;
        image.file(&output.bytes(&disc)?)?;
    }
    image.finish()?;

    staged.finish()
}

/// Opens a project and the disc it came out of.
///
/// Both endings copy untouched files straight off that disc, so it has to be
/// findable and has to still be the dump the project was unpacked from.
/// Anything else would quietly mix two prints together.
fn open(
    project: &Path,
) -> Result<(Metadata, Disc, std::collections::HashMap<String, String>), Error> {
    let metadata = Manifest::read(project)?;
    let store = Store::new(project);
    let source = store.source()?;

    if !source.path.exists() {
        return Err(Error::SourceMissing(source.path));
    }
    let disc = Disc::open(&source.path)?;
    if disc.sha1()? != source.sha1 {
        return Err(Error::SourceChanged(source.path));
    }

    let vanilla = store.hashes()?;
    Ok((metadata, disc, vanilla))
}

/// Where a finished mod or image goes: exactly where it was asked for, or
/// `name` under the project's own output directory.
///
/// A named path is taken whole, including the last component, so `-o test.iso`
/// produces a test.iso rather than a test.iso holding something else.
fn target(project: &Path, out: Option<&Path>, name: String) -> PathBuf {
    match out {
        Some(out) => out.to_path_buf(),
        None => project.join(OUT).join(name),
    }
}

/// What a build names its output after, which is the print rather than the
/// project, since that is what an image or a mod is of.
fn print(metadata: &Metadata) -> String {
    format!("{}-rev{}", metadata.boot.id, metadata.boot.revision)
}

/// Which format crate a file's bytes belong to. Nothing outside this
/// type refers to a format crate by name.
enum Format {
    /// A RARC archive. Not a file on its own: unpacked into a directory of
    /// its own members rather than parsed and written through.
    Archive,
    /// A BMG message file.
    Bmg,
    /// Nothing here has an opinion about this one, so its bytes go straight
    /// through untouched.
    Other,
}

impl Format {
    fn of(path: &str) -> Self {
        match path.rsplit_once('.').map(|(_, extension)| extension) {
            Some("arc") => Self::Archive,
            Some("bmg") => Self::Bmg,
            _ => Self::Other,
        }
    }

    /// Decodes `bytes` into this format's editable form, when it has one.
    ///
    /// `None` means there is nothing to convert: the bytes go through
    /// unchanged. `Some` carries the editable bytes and the extension a
    /// project chains onto the file's own to hold them.
    fn decode(&self, bytes: &[u8], path: &Path) -> Result<Option<(&'static str, Vec<u8>)>, Error> {
        match self {
            Format::Bmg => match tpmt_bmg::unpack(bytes) {
                Ok(bmg) => Ok(Some((
                    tpmt_bmg::editable::json::EXTENSION,
                    tpmt_bmg::editable::json::encode(&bmg),
                ))),
                Err(tpmt_bmg::Error::NotBmg) => Ok(None),
                Err(source) => Err(Error::Bmg {
                    path: path.to_path_buf(),
                    source,
                }),
            },
            Format::Archive => unreachable!("archives are unpacked before reaching this"),
            Format::Other => Ok(None),
        }
    }
}

/// Sends bytes to whichever unpacker their format wants: an archive unpacks
/// into a directory of its own, anything else goes through its format crate
/// and is written to `path` as-is.
fn unpack_format(
    format: Format,
    bytes: &[u8],
    path: &Path,
    at: &str,
    hashes: &mut Vec<FileHash>,
) -> Result<(), Error> {
    match format {
        Format::Archive => unpack_archive(bytes, path, at, hashes),
        format => unpack_file(format, bytes, path, at, hashes),
    }
}

/// Unpacks one disc entry: an archive becomes a directory of its contents,
/// anything else becomes a file of the bytes the disc holds.
///
/// Gives back the hash of everything it wrote, keyed by where it wrote it,
/// which is what a build later compares against to find the edits.
///
/// A directory entry only matters when it is empty. Anything with members gets
/// created on the way past by the members themselves.
fn unpack_disc_entry(disc: &Disc, entry: &Entry, project: &Path) -> Result<Vec<FileHash>, Error> {
    let path = project.join(entry.path());
    let Entry::File { offset, size, .. } = *entry else {
        fs::create_dir(&path)?;
        return Ok(Vec::new());
    };

    let bytes = disc.read(offset, size)?;
    let mut hashes = Vec::new();
    unpack_format(
        Format::of(entry.path()),
        &bytes,
        &path,
        entry.path(),
        &mut hashes,
    )?;
    Ok(hashes)
}

/// Unpacks an archive into a directory named after it.
///
/// Keeping the `.arc` on the directory is what lets build recognise it later
/// without a note anywhere saying so.
///
/// That an archive on this disc is Yaz0 wrapped around RARC is known here and
/// nowhere else: the compression is a fact about where the file sits, not
/// something either format has an opinion about. Archives nested inside
/// archives are wrapped the same way and unpack the same way.
///
/// A wrapper found on an arc (and its entries) is taken off too.
/// Format crates expect the format bytes, not wrapped in something unexpected.
/// A sidecar notes wrapping down per member and a build puts it back.
///
/// Everything the files themselves cannot say goes in the sidecar at the root
/// of the directory: the root name, the member order, which memory each member
/// belongs in, and which wrappers came off. A rebuild works off that, so it
/// does not need the disc to remember what this archive was.
///
/// Not every `.arc` is one of ours. A handful hold other container formats
/// entirely, and those stay whole, exactly as the disc had them, rather than
/// being half-understood.
///
/// The sidecar is built here, not in `tpmt-arc`: a member's wrapping state
/// is only known once it's gone through its own format crate, and `tpmt-arc`
/// can't see other format crates without depending on them.
fn unpack_archive(
    bytes: &[u8],
    path: &Path,
    at: &str,
    hashes: &mut Vec<FileHash>,
) -> Result<(), Error> {
    let yaz0_compressed = tpmt_compress::is_yaz0(bytes);
    let contents = match yaz0_compressed {
        true => Cow::Owned(decompress(bytes, path)?),
        false => Cow::Borrowed(bytes),
    };

    let unpacked = match tpmt_arc::unpack(&contents) {
        Ok(unpacked) => unpacked,
        // The original bytes, not the decompressed ones: what is written here
        // has to be what goes back on the disc.
        Err(tpmt_arc::Error::NotRarc) => {
            fs::write(path, bytes)?;
            hashes.push(FileHash {
                path: at.to_string(),
                digest: plan::hash(bytes),
            });
            return Ok(());
        }
        Err(source) => {
            return Err(Error::Archive {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    fs::create_dir(path)?;
    let mut members = Vec::with_capacity(unpacked.files.len());
    for file in unpacked.files {
        let inside = format!("{at}/{}", file.path);
        let member = path.join(&file.path);

        let format = Format::of(&file.path);

        // A nested archive keeps its own wrapping in its own sidecar, so the
        // one entry that stays false here is the one that has somewhere better
        // to be, and it is also why an archive gets its bytes raw here rather
        // than through the decompression below: it does that unwrapping itself.
        let mut yaz0_compressed = false;
        let data = match format {
            Format::Archive => Cow::Borrowed(file.data),
            _ => {
                yaz0_compressed = tpmt_compress::is_yaz0(file.data);
                match yaz0_compressed {
                    true => Cow::Owned(decompress(file.data, &member)?),
                    false => Cow::Borrowed(file.data),
                }
            }
        };
        // Handle unpack for nested archive or files
        unpack_format(format, &data, &member, &inside, hashes)?;

        members.push(tpmt_arc::editable::sidecar::Member {
            path: file.path,
            preload: file.preload,
            yaz0_compressed,
            id: file.id,
        });
    }

    let sidecar =
        tpmt_arc::editable::sidecar::Sidecar::new(unpacked.root, yaz0_compressed, members);
    let written = write_sidecar(&sidecar, path)?;
    hashes.push(FileHash {
        path: format!("{at}/{}", tpmt_arc::editable::sidecar::SIDECAR),
        digest: plan::hash(&written),
    });

    Ok(())
}

fn decompress(bytes: &[u8], path: &Path) -> Result<Vec<u8>, Error> {
    tpmt_compress::yaz0_decode(bytes).map_err(|source| Error::Compress {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn compress(bytes: &[u8], path: &Path) -> Result<Vec<u8>, Error> {
    tpmt_compress::yaz0_encode(bytes, false).map_err(|source| Error::Compress {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes a file into the project and records its hash for change detection.
///
/// A format with an editable form gets that instead, chained onto the file's
/// own extension (`zel_00.bmg` becomes `zel_00.bmg.json`); anything else goes
/// through as the bytes it already had.
fn unpack_file(
    format: Format,
    bytes: &[u8],
    path: &Path,
    at: &str,
    hashes: &mut Vec<FileHash>,
) -> Result<(), Error> {
    match format.decode(bytes, path)? {
        Some((extension, editable)) => fs::write(&chained(path, extension), &editable)?,
        None => fs::write(path, bytes)?,
    }
    hashes.push(FileHash {
        path: at.to_string(),
        digest: plan::hash(bytes),
    });
    Ok(())
}

/// `path` with `extension` appended after its own, rather than replacing it.
fn chained(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

pub(crate) fn is_archive(path: &str) -> bool {
    matches!(Format::of(path), Format::Archive)
}

/// Every disc file's offset and size, keyed by its disc path, for a caller
/// that needs one back out by name rather than walking every entry.
pub(crate) fn on_disc(entries: &[Entry]) -> std::collections::HashMap<&str, (u64, u64)> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::File { path, offset, size } => Some((path.as_str(), (*offset, *size))),
            Entry::Directory { .. } => None,
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not a Twilight Princess disc")]
    UnsupportedDisc(String),

    #[error("`{}` is already a project; pass overwrite to replace it", .0.display())]
    ProjectExists(PathBuf),

    /// Raised by `Manifest::read` when `tpmt.toml` is missing at a root
    /// `discover` already confirmed, meaning the project itself is broken
    /// (or, before discovery ran, the only check `unpack`'s caller had).
    #[error("`{}` is not a project: it has no tpmt.toml", .0.display())]
    NotAProject(PathBuf),

    /// Raised by `discover` when no `.tpmt` turned up walking upward at all,
    /// meaning the search never found anywhere worth checking for a
    /// `tpmt.toml` in the first place.
    #[error("`{}` is not inside a tpmt project", .0.display())]
    NoProjectFound(PathBuf),

    /// Raised when something is already sitting where an unpack or a build
    /// would write, and it was not put there by an earlier run of this tool.
    /// Refusing to overwrite means a person's own files never vanish just for
    /// being in the way.
    #[error("`{}` holds something this did not write, so it will not be replaced", .0.display())]
    ForeignDirectory(PathBuf),

    #[error("`{}` is not where it was when the project was unpacked", .0.display())]
    SourceMissing(PathBuf),

    #[error("`{}` is not the disc this project was unpacked from", .0.display())]
    SourceChanged(PathBuf),

    /// Raised when a generated file under the project only this tool ever
    /// writes does not parse in the shape this tool always writes it in, which
    /// means somebody or something else got to it first.
    #[error("`{}` is not readable as something this wrote", .0.display())]
    CorruptStore(PathBuf),

    #[error("`{0}` is not on the source disc, so there is nothing to copy from")]
    NotOnDisc(String),

    #[error("`{0}` is not part of this project, so there is nothing to revert")]
    NotTracked(String),

    #[error("`{}` is outside the project", .0.display())]
    OutsideProject(PathBuf),

    /// `repack` in plan.rs catches this and defaults instead when there is no
    /// original to compare against.
    #[error("`{}` is missing, so there is nothing saying what archive this is", .0.display())]
    LostSidecar(PathBuf),

    #[error("`{}`: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error("`{}`: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },

    #[error("`{}`: {source}", .path.display())]
    Compress {
        path: PathBuf,
        source: tpmt_compress::Error,
    },

    #[error("`{}`: {source}", .path.display())]
    Archive {
        path: PathBuf,
        source: tpmt_arc::Error,
    },

    #[error("`{}`: {source}", .path.display())]
    Bmg {
        path: PathBuf,
        source: tpmt_bmg::Error,
    },

    #[error("the project file could not be written: {0}")]
    Project(#[from] toml::ser::Error),

    #[error("`{}` could not be read: {source}", .path.display())]
    UnreadableProject {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error(transparent)]
    Disc(#[from] tpmt_disc::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn finds_the_root_from_itself() {
        let scratch = Scratch::new("self");
        fs::create_dir_all(scratch.0.join(store::STORE)).unwrap();

        let found = discover(&scratch.0).unwrap();
        assert_eq!(found, scratch.0.canonicalize().unwrap());
    }

    #[test]
    fn finds_the_root_from_a_subdirectory() {
        let scratch = Scratch::new("nested");
        fs::create_dir_all(scratch.0.join(store::STORE)).unwrap();
        let nested = scratch.0.join(FILES).join("thing.arc");
        fs::create_dir_all(&nested).unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, scratch.0.canonicalize().unwrap());
    }

    #[test]
    fn refuses_a_directory_with_no_project_above_it() {
        let scratch = Scratch::new("none");

        let error = discover(&scratch.0).unwrap_err();
        assert!(matches!(error, Error::NoProjectFound(_)));
    }

    /// `unpack` never gets as far as touching a directory nothing here wrote,
    /// so a personal folder that happens to share a name is left alone rather
    /// than cleared to make room.
    #[test]
    fn refuses_to_unpack_over_a_directory_that_is_not_a_project() {
        let scratch = Scratch::new("foreign");
        let target = scratch.0.join("mine");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("notes.txt"), b"do not delete").unwrap();

        let error = unpack(Path::new("nonexistent.iso"), &target, true).unwrap_err();
        assert!(matches!(error, Error::ForeignDirectory(path) if path == target));
        assert!(target.join("notes.txt").exists());
    }

    /// An empty directory has nothing in it to protect, so it is treated the
    /// same as one that does not exist yet rather than refused as foreign.
    #[test]
    fn unpacks_into_a_directory_that_exists_but_is_empty() {
        let scratch = Scratch::new("empty");
        let target = scratch.0.join("mine");
        fs::create_dir_all(&target).unwrap();

        let error = unpack(Path::new("nonexistent.iso"), &target, false).unwrap_err();
        assert!(!matches!(
            error,
            Error::ForeignDirectory(_) | Error::ProjectExists(_)
        ));
    }

    /// Unpacking again on top of an existing project needs `overwrite` said
    /// explicitly; the CLI turns that into a prompt, but the library itself
    /// never overwrites on its own say-so.
    #[test]
    fn refuses_to_unpack_over_a_project_without_overwrite() {
        let scratch = Scratch::new("existing");
        let target = scratch.0.join("project");
        fs::create_dir_all(target.join(store::STORE)).unwrap();

        let error = unpack(Path::new("nonexistent.iso"), &target, false).unwrap_err();
        assert!(matches!(error, Error::ProjectExists(path) if path == target));
    }

    /// A member whose extension picks a format crate, but whose bytes do not
    /// actually match it, writes through unremarked rather than being
    /// stopped over: the extension is a guess at what to check, not a claim
    /// enforced on every file wearing it.
    ///
    /// BMG stands in here for any format crate wired into `Format`; an `.arc`
    /// holding something other than RARC takes the same path, etc...
    #[test]
    fn a_mismatched_format_member_writes_through() {
        let scratch = Scratch::new("notbmg");
        let archive = tpmt_arc::pack(&tpmt_arc::Archive {
            root: "archive".to_string(),
            files: vec![tpmt_arc::File {
                path: "fake.bmg".to_string(),
                data: b"not a message file",
                id: None,
                preload: tpmt_arc::Preload::Mram,
            }],
            ..Default::default()
        })
        .unwrap();

        let mut hashes = Vec::new();
        let at = scratch.0.join("thing.arc");
        unpack_archive(&archive, &at, "files/thing.arc", &mut hashes).unwrap();

        assert_eq!(
            crate::fs::read(&at.join("fake.bmg")).unwrap(),
            b"not a message file"
        );
    }

    /// The bytes of a one-message BMG file: a header naming its two sections,
    /// an INF1 with a single record, and a DAT1 holding the text it points at.
    fn minimal_bmg() -> Vec<u8> {
        let mut inf1_body = Vec::new();
        inf1_body.extend(1u16.to_be_bytes()); // record count
        inf1_body.extend(4u16.to_be_bytes()); // record_len: a bare text offset, no attributes
        inf1_body.extend([0u8; 4]); // group id + padding, neither read
        inf1_body.extend(0u32.to_be_bytes()); // the one record's text offset into DAT1

        let dat1_body = b"Hi\0".to_vec();

        let mut sections = Vec::new();
        sections.extend(b"INF1");
        sections.extend((8 + inf1_body.len() as u32).to_be_bytes());
        sections.extend(&inf1_body);
        sections.extend(b"DAT1");
        sections.extend((8 + dat1_body.len() as u32).to_be_bytes());
        sections.extend(&dat1_body);

        let mut bmg = Vec::new();
        bmg.extend(*b"MESGbmg1");
        bmg.extend((0x20u32 + sections.len() as u32).to_be_bytes());
        bmg.extend(2u32.to_be_bytes()); // section count
        bmg.push(0x03); // Shift-JIS
        bmg.resize(0x20, 0);
        bmg.extend(&sections);
        bmg
    }

    /// A member whose format converts is written exactly as its format crate
    /// handed it back, at the extension that came with it, and not at its own
    /// original path. What the conversion actually contains is that format
    /// crate's own concern; this only checks that `unpack_file` moves what it
    /// is given rather than reinterpreting it.
    ///
    /// BMG stands in here for any format crate whose bytes convert, the same
    /// way it stands in for one that doesn't in
    /// `a_mismatched_format_member_writes_through`.
    #[test]
    fn a_converted_member_is_written_at_its_chained_extension() {
        let scratch = Scratch::new("converted");
        let bmg = minimal_bmg();
        let at = scratch.0.join("message.bmg");
        let (extension, expected) = Format::Bmg.decode(&bmg, &at).unwrap().unwrap();

        let mut hashes = Vec::new();
        unpack_file(Format::Bmg, &bmg, &at, "files/message.bmg", &mut hashes).unwrap();

        assert!(!at.exists());
        assert_eq!(crate::fs::read(&chained(&at, extension)).unwrap(), expected);
    }
}
