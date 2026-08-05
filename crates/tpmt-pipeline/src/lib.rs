//! Unpacking a disc to a project folder and building it back.
//!
//! Unpacking walks the disc, peels off compression, opens archives, hands decodable
//! files to whichever format crate claims them, and writes the result out as a single
//! folder a person edits in place. Building runs the same route backwards.
//!
//! Building has two endings. A build works out what changed, re-encodes it, and
//! gathers the finished game files into a mod somebody else can install. An
//! image carries on from there and lays those files out as a disc, which is the
//! only step of the two that has to know where anything goes. Everything before
//! that split is one path, and neither ending is built by way of the other.
//!
//! Edits are never tracked as they happen. Which files a person touched is
//! worked out at build time, by hashing them against the vanilla hashes taken
//! at unpack, and only those get re-encoded. Everything else is copied out of
//! the source disc verbatim as raw bytes.
//!
//! Rebuilding an archive does not mean rebuilding what is inside it. An archive
//! nobody touched is copied off the disc whole, still compressed. An archive
//! counts as modified when any member was edited, added or deleted, and only
//! then is it repacked and recompressed. Even so, only the added and edited
//! members are re-encoded: every member left alone is pulled verbatim off the
//! source disc rather than rebuilt, and a deleted member is simply not pulled
//! at all. No file is ever put back through a codec because something sitting
//! next to it changed.
//!
//! The only crate allowed to know how formats stack. That a `.arc` on the disc
//! is usually Yaz0 wrapped around RARC is a fact about this pipeline, not about
//! either format.
//!
//! Format crates convert their own file type to an editable form and back.
//! Which form that is belongs to the crate rather than here, so this one only
//! asks whether a conversion exists and takes back what it produces, extension
//! included. Their typed models are public as well, reachable by an editor
//! without a file in between.
//!
//! Unpack fans out one flat layer over FST entries, each self-contained.

// Project, one directory, edited in place:
//   tpmt.toml   schema version, the preamble values a build cannot derive
//   sys/        apploader.img, main.dol
//   files/      game content, archives as directories
//   out/        built mods and images
//
// Store, everything we generate about it rather than for it:
//   .tpmt/source.toml   where the ISO was last seen, plus its sha1
//   .tpmt/hashes        vanilla hashes, for change detection
//
// Paths mirror the disc, decoded files chain extensions (zel_00.bmg.json).

// TODO: the golden roundtrip. The end to end fidelity test: unpack a retail
// ISO, image it straight back with nothing edited, and require every entry the
// disc reports to come back byte for byte. It lives at this level because only
// the pipeline sees the whole path (disc, compression, archives, formats); the
// format crates each proved their own fidelity with throwaway probes, and this
// is the committed test that keeps all of it true at once. Needs an ISO on hand,
// so it runs as an ignored integration test under tests/ pointed at assets/. It
// goes through image, since that is the one that produces something a disc
// reader can be pointed back at, and getting there means every step build owns
// was already right.
//
// It compares entries, not whole files: an image drops the mastering fill (see
// tpmt-disc) and so is smaller than its source. assets/NA.iso is already
// scrubbed, so it is the one print where comparing the files would pass anyway.
// Do not calibrate on it.
//
// The file table is the exception worth checking whole. It is never unpacked,
// so an image derives it from the project tree, and the source disc still has
// the original to hold that against. Only the offsets are allowed to differ.

// TODO: routing tables (disc path to project path) are still hardcoded nowhere.
// Scope is Twilight Princess only, but GZ2E, GZ2P and GZ2J do not share paths,
// so whatever holds them is keyed by region.

// TODO: hand files to the format crates. Every file currently lands as the
// bytes the disc holds, so a `.bmg` is still a `.bmg` rather than the JSON a
// person would edit. Nothing else has to move for that: an unpacked file is
// hashed as it was written, and a build already asks for the bytes that go on
// the disc rather than the bytes on the filesystem.

// TODO: adding or removing an archive member. Both are refused by name, since
// rebuilding the tables it would take is the re-emitter marked TODO in
// tpmt-arc. Adding or removing a file on the disc itself already works.

// TODO: decide whether to keep our own copy of the ISO rather than remembering
// its originating path. Either way, tell the user before taking their disk space.

// TODO: a mod does not say a file was deleted, only which ones it replaces or
// adds. An image handles a deletion fine, since it lays out whatever the tree
// holds.

mod plan;
mod project;
mod store;

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tpmt_disc::{BI2_PATH, BOOT_PATH, Disc, Entry, Layout, Metadata};

use crate::plan::Output;
use crate::project::{FILES, OUT, Project, SYS, Staged};
use crate::store::{Source, Store};

/// The three Twilight Princess prints. Any other disc would unpack fine and
/// then mean nothing to the rest of the toolkit, so it is turned away here.
const SUPPORTED_IDS: [&str; 3] = ["GZ2E", "GZ2P", "GZ2J"];

/// Unpacks a disc image into a new project directory.
///
/// The directory is created here and is expected not to exist yet. Reusing one
/// would mean reconciling whatever edits are already sitting in it, which is
/// what `build` is for.
pub fn unpack(iso: &Path, project: &Path) -> Result<(), Error> {
    let disc = Disc::open(iso)?;
    let metadata = disc.metadata();
    let id = &metadata.boot.id;
    if !SUPPORTED_IDS.contains(&id.as_str()) {
        return Err(Error::UnsupportedDisc(id.clone()));
    }

    if project.exists() {
        return Err(Error::ProjectExists(project.to_path_buf()));
    }
    // Nothing to allow: the check above already refused a directory that exists
    // at all, so this one never replaces anything.
    let staged = Staged::directory(project, &[])?;
    let at = staged.path();
    Project::new(metadata).write(at)?;

    // One flat layer of work. Every entry reads its own bytes off the shared
    // disc and writes its own outputs, so there is nothing to hand between
    // threads and nothing to order them by.
    let hashes: Vec<(String, String)> = disc
        .entries()?
        .par_iter()
        .map(|entry| unpack_entry(&disc, entry, at))
        .collect::<Result<Vec<_>, Error>>()?
        .concat();

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
    store.write_hashes(&hashes)?;

    staged.finish()?;
    Ok(())
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
        project::write(&staged.path().join(&output.path), &output.bytes(&disc)?)?;
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
            project::write(&at.join(path), &built)?;
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
    let metadata = Project::read(project)?;
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

/// Unpacks one disc entry: an archive becomes a directory of its contents,
/// anything else becomes a file of the bytes the disc holds.
///
/// Gives back the hash of everything it wrote, keyed by where it wrote it,
/// which is what a build later compares against to find the edits.
///
/// A directory entry only matters when it is empty. Anything with members gets
/// created on the way past by the members themselves.
fn unpack_entry(
    disc: &Disc,
    entry: &Entry,
    project: &Path,
) -> Result<Vec<(String, String)>, Error> {
    let path = project.join(entry.path());
    let Entry::File { offset, size, .. } = *entry else {
        project::create_dir(&path)?;
        return Ok(Vec::new());
    };

    let bytes = disc.read(offset, size)?;
    let mut hashes = Vec::new();
    match is_archive(entry.path()) {
        true => unpack_archive(&bytes, &path, entry.path(), &mut hashes)?,
        false => {
            project::write(&path, &bytes)?;
            hashes.push((entry.path().to_string(), plan::hash(&bytes)));
        }
    }
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
/// Whether the wrapper was there is not recorded, because it does not have to
/// be: a build that repacks this archive reads the original first, and the
/// original says.
///
/// Not every `.arc` is one of ours. A handful hold other container formats
/// entirely, and those stay whole, exactly as the disc had them, rather than
/// being half-understood.
fn unpack_archive(
    bytes: &[u8],
    path: &Path,
    at: &str,
    hashes: &mut Vec<(String, String)>,
) -> Result<(), Error> {
    let contents = match tpmt_compress::is_yaz0(bytes) {
        true => {
            Cow::Owned(
                tpmt_compress::yaz0_decode(bytes).map_err(|source| Error::Compress {
                    path: path.to_path_buf(),
                    source,
                })?,
            )
        }
        false => Cow::Borrowed(bytes),
    };

    let files = match tpmt_arc::files(&contents) {
        Ok(files) => files,
        // The original bytes, not the decompressed ones: what is written here
        // has to be what goes back on the disc.
        Err(tpmt_arc::Error::NotRarc) => {
            project::write(path, bytes)?;
            hashes.push((at.to_string(), plan::hash(bytes)));
            return Ok(());
        }
        Err(source) => {
            return Err(Error::Archive {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    project::create_dir(path)?;
    for file in files {
        let inside = format!("{at}/{}", file.path);
        let path = path.join(&file.path);
        match is_archive(&file.path) {
            true => unpack_archive(file.data, &path, &inside, hashes)?,
            false => {
                project::write(&path, file.data)?;
                hashes.push((inside, plan::hash(file.data)));
            }
        }
    }

    Ok(())
}

fn is_archive(path: &str) -> bool {
    path.ends_with(".arc")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` is not a Twilight Princess disc")]
    UnsupportedDisc(String),

    #[error("`{}` already exists, so there is nothing to unpack into", .0.display())]
    ProjectExists(PathBuf),

    #[error("`{}` is not a project: it has no tpmt.toml", .0.display())]
    NotAProject(PathBuf),

    #[error("`{}` holds something this did not write, so it will not be replaced", .0.display())]
    NotOurs(PathBuf),

    #[error("this project was written by schema {found}, and this is schema {want}")]
    Schema { found: u32, want: u32 },

    #[error("`{}` is not where it was when the project was unpacked", .0.display())]
    SourceMissing(PathBuf),

    #[error("`{}` is not the disc this project was unpacked from", .0.display())]
    SourceChanged(PathBuf),

    #[error("`{}` is not readable as something this wrote", .0.display())]
    CorruptStore(PathBuf),

    #[error("`{0}` is not on the source disc, so there is nothing to copy from")]
    NotOnDisc(String),

    #[error("`{0}` is a new archive, and building one from nothing is not supported yet")]
    NewArchive(String),

    #[error("`{0}` is new to its archive, and adding a member is not supported yet")]
    AddedMember(String),

    #[error("`{0}` is gone from its archive, and removing a member is not supported yet")]
    DeletedMember(String),

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
