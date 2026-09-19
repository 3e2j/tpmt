//! The project directory: where everything in it lives, the fs helpers that
//! get bytes onto disk under it, and the store that marks one as a finished
//! unpack.
//!
//! A project is two directories, edited in place:
//!
//! ```text
//! base/         read-only unpack of the ISO, decoded index for the UI to browse
//!   disc.toml   the preamble values a build cannot derive
//!   yaz0.toml   which loose files arrived Yaz0 wrapped
//!   sys/        apploader.img, main.dol
//!   files/      game content, archives as directories
//! mod/          the mod project; the only directory a modder edits
//!   overlay/    whole-file / archive-member replacements, real paths
//!   res/        authored user-made content
//!     scripts/  Luau scripts
//!   mod.json    mod metadata (id, name, version, author, description, icon, banner)
//! build/        what `build` and `image` produce, made by the first of them
//! ```
//!
//! `base/` is rewritten whole on every unpack. `mod/` is scaffolded once.
//!
//! Facts a decoded file can't carry, like a wrapper that came off it or
//! which memory an archive member loads into, live in a sidecar next to it
//! instead, one name per format:
//!
//! ```text
//! *.arc/.tpmt-arc.toml   what an unpacked archive is, minus its bytes
//! ```
//!
//! Everything generated about the project, rather than for it, lives in the
//! store:
//!
//! ```text
//! .tpmt/source.toml   where the ISO was last seen, plus its sha1
//! .tpmt/hashes.toml   vanilla hashes of base/, for change detection
//! ```
//!
//! The store is written at [`crate::unpack`] and read back by `status`/`build`.
//! Never hand-edited, so its shape is whatever's convenient to (de)serialize,
//! not whatever reads best by hand. It goes in last, after everything else
//! succeeded, so its presence means an unpack finished: [`is_project`]
//! is exactly that test.
// # TODO
// Disc paths (from [`tpmt_disc::Entry`]) are used as project paths verbatim.
// That holds for one disc, but GZ2E, GZ2P and GZ2J do not share paths, so a
// routing table keyed by region will be needed once anything has to
// reconcile more than one disc against a project.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Error, Result};

// Base game
/// Read-only unpack of the game.
pub const BASE_DIR: &str = "base";
/// Where [`Staging`] writes a fresh unpack before promoting it to [`BASE_DIR`].
const BASE_TMP_DIR: &str = "base.tmp";
/// The disc preamble values a build cannot derive, under [`BASE_DIR`].
pub const DISC_TOML: &str = "disc.toml";
/// Which loose files arrived Yaz0 wrapped, under [`BASE_DIR`]: see [`Yaz0`].
pub const YAZ0_TOML: &str = "yaz0.toml";

// Mod (authored) directory
/// The mod project: `overlay/`, `res/`, `mod.json`.
pub const MOD_DIR: &str = "mod";
const OVERLAY_DIR: &str = "overlay";
const RES_DIR: &str = "res";
const SCRIPTS_DIR: &str = "scripts";
const MOD_JSON: &str = "mod.json";

// TPMT specifics
const STORE_DIR: &str = ".tpmt";
const HASHES_TOML: &str = "hashes.toml";
const SOURCE_TOML: &str = "source.toml";

/// `yaz0.toml`: which loose files arrived Yaz0 wrapped. Recorded here
/// because a file never records its own wrapper: the container holding it
/// does, and for a loose file that is the disc.
#[derive(Serialize)]
struct Yaz0<'a> {
    compressed: &'a [String],
}

/// Where the ISO this project came from was last seen, and its sha1, so a
/// build can tell if it moved or changed.
#[derive(Serialize)]
struct Source<'a> {
    iso: &'a Path,
    sha1: &'a str,
}

/// `mod.json`'s starter shape.
///
/// Fields are the target-agnostic subset only. Dusklight reads a few more
/// (`runtime`, pinning a mod to a specific host runtime service) that are
/// specific to the `.dusk` export step.
#[derive(Serialize)]
struct ModMetadata<'a> {
    id: &'a str,
    name: &'a str,
    version: &'a str,
    author: &'a str,
    description: &'a str,
    icon: Option<&'a str>,
    banner: Option<&'a str>,
}

/// Whether `dir` is a finished unpack: it has the store that only
/// [`commit`] writes, and only after everything else is in place.
#[must_use]
pub fn is_project(dir: &Path) -> bool {
    dir.join(STORE_DIR).is_dir()
}

/// Finds the project root by walking upward from `start`, the way git finds
/// `.git`: canonicalize first, then climb one directory at a time until a
/// `.tpmt` store turns up or the filesystem root is reached.
///
/// # Errors
///
/// - [`Error::Io`] if `start` cannot be canonicalized
/// - [`Error::NoProjectFound`] if nothing above `start` has a `.tpmt` store
pub fn discover(start: &Path) -> Result<PathBuf> {
    let mut at = start.canonicalize().map_err(io_at(start))?;

    loop {
        if is_project(&at) {
            return Ok(at);
        }
        if !at.pop() {
            return Err(Error::NoProjectFound(start.to_path_buf()));
        }
    }
}

/// Refuses to unpack into a directory holding something this crate did not
/// write. A project, an empty directory, or no directory at all is fine.
pub fn refuse_foreign(project: &Path) -> Result<()> {
    if is_project(project) || !project.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(project).map_err(io_at(project))?;
    if entries.next().is_some() {
        return Err(Error::ForeignDirectory(project.to_path_buf()));
    }
    Ok(())
}

/// A fresh `base/` being written under a temporary name, swapped in by
/// [`promote`](Self::promote) once complete and thrown away otherwise.
///
/// Dropping one unpromoted removes whatever it wrote, so a failed unpack
/// leaves the project as it found it.
pub struct Staging {
    dir: PathBuf,
}

impl Staging {
    /// Clears anything a previous failed unpack left behind and opens a
    /// fresh staging directory.
    pub fn begin(project: &Path) -> Result<Self> {
        let dir = project.join(BASE_TMP_DIR);
        remove_dir_all_if_exists(&dir)?;
        create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Where the unpack writes `base/`'s contents.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Swaps the staged tree in as `base/`. The old `base/` is moved aside
    /// first so no point in the swap has neither.
    pub fn promote(self) -> Result<()> {
        let project = self.dir.parent().unwrap_or(&self.dir);
        let base = project.join(BASE_DIR);
        let old = project.join(format!("{BASE_DIR}.old"));

        remove_dir_all_if_exists(&old)?;
        if base.exists() {
            fs::rename(&base, &old).map_err(io_at(&base))?;
        }
        fs::rename(&self.dir, &base).map_err(io_at(&base))?;
        remove_dir_all_if_exists(&old)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Best-effort: after a promote there is nothing here; after a
        // failure, the error that caused it is the one that matters.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Writes the `mod/` skeleton (`overlay/`, `res/scripts/`, a starter
/// `mod.json`) alongside `base/`. Left untouched if `mod/` already exists,
/// so re-unpacking a project never clobbers a modder's own edits.
pub fn scaffold_mod(project: &Path) -> Result<()> {
    let mod_dir = project.join(MOD_DIR);
    if mod_dir.is_dir() {
        return Ok(());
    }

    create_dir_all(&mod_dir.join(OVERLAY_DIR))?;
    create_dir_all(&mod_dir.join(RES_DIR).join(SCRIPTS_DIR))?;

    let id = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mod");
    write_json(
        &mod_dir.join(MOD_JSON),
        &ModMetadata {
            id,
            name: id,
            version: "0.1.0",
            author: "",
            description: "",
            icon: None,
            banner: None,
        },
    )
}

/// `disc.toml`: the preamble values a build cannot derive.
pub fn write_metadata(base: &Path, metadata: &tpmt_disc::Metadata) -> Result<()> {
    write_toml(&base.join(DISC_TOML), metadata)
}

/// `yaz0.toml`: see [`Yaz0`].
pub fn write_yaz0(base: &Path, compressed: &[String]) -> Result<()> {
    write_toml(&base.join(YAZ0_TOML), &Yaz0 { compressed })
}

/// Writes the store, which is what makes `project` a project. Only called
/// once every other file is in place.
///
/// The ISO path is stored canonical: a project is often built from
/// somewhere other than where it was unpacked.
pub fn commit(
    project: &Path,
    iso: &Path,
    sha1: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<()> {
    let iso = iso.canonicalize().map_err(io_at(iso))?;
    let store = project.join(STORE_DIR);
    write_toml(&store.join(HASHES_TOML), hashes)?;
    write_toml(&store.join(SOURCE_TOML), &Source { iso: &iso, sha1 })
}

pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io_at(path))
}

/// Writes `data` to `path`, creating whatever directories it takes to get
/// there.
pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    fs::write(path, data).map_err(io_at(path))
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

/// Like [`fs::remove_dir_all`], but a missing `path` is not an error: there
/// is already nothing there to clear.
fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A scratch directory, gone again when the test that made it ends.
    /// Tagged with a counter as well as a name, since two tests picking the
    /// same name would otherwise race on one path.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            static CALLS: AtomicU64 = AtomicU64::new(0);
            let unique = CALLS.fetch_add(1, Ordering::Relaxed);
            let at = std::env::temp_dir()
                .join(format!("tpmt-test-{name}-{}-{unique}", std::process::id()));
            let _ = fs::remove_dir_all(&at);
            fs::create_dir_all(&at).unwrap();
            Self(at)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mark_project(dir: &Path) {
        fs::create_dir_all(dir.join(STORE_DIR)).unwrap();
    }

    #[test]
    fn finds_the_root_from_itself() {
        let scratch = Scratch::new("self");
        mark_project(&scratch.0);

        let found = discover(&scratch.0).unwrap();
        assert_eq!(found, scratch.0.canonicalize().unwrap());
    }

    #[test]
    fn finds_the_root_from_a_subdirectory() {
        let scratch = Scratch::new("nested");
        mark_project(&scratch.0);
        let nested = scratch.0.join(BASE_DIR).join("files").join("thing.arc");
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

    /// A personal folder that happens to share a name is left alone rather
    /// than cleared to make room.
    #[test]
    fn refuses_a_directory_that_is_not_a_project() {
        let scratch = Scratch::new("foreign");
        let target = scratch.0.join("mine");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("notes.txt"), b"do not delete").unwrap();

        let error = refuse_foreign(&target).unwrap_err();
        assert!(matches!(error, Error::ForeignDirectory(path) if path == target));
        assert!(target.join("notes.txt").exists());
    }

    /// An empty directory has nothing in it to protect, and a project is
    /// what an unpack is for.
    #[test]
    fn accepts_empty_missing_and_project_directories() {
        let scratch = Scratch::new("accepted");
        let empty = scratch.0.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let project = scratch.0.join("project");
        mark_project(&project);
        fs::write(project.join("anything"), b"").unwrap();

        refuse_foreign(&empty).unwrap();
        refuse_foreign(&scratch.0.join("missing")).unwrap();
        refuse_foreign(&project).unwrap();
    }

    #[test]
    fn dropped_staging_leaves_no_trace() {
        let scratch = Scratch::new("staging-drop");
        let staging = Staging::begin(&scratch.0).unwrap();
        fs::write(staging.dir().join("half"), b"written").unwrap();
        drop(staging);

        assert!(!scratch.0.join(BASE_TMP_DIR).exists());
        assert!(!scratch.0.join(BASE_DIR).exists());
    }

    #[test]
    fn promoted_staging_replaces_base() {
        let scratch = Scratch::new("staging-promote");
        let base = scratch.0.join(BASE_DIR);
        write(&base.join("stale"), b"old").unwrap();

        let staging = Staging::begin(&scratch.0).unwrap();
        write(&staging.dir().join("fresh"), b"new").unwrap();
        staging.promote().unwrap();

        assert_eq!(fs::read(base.join("fresh")).unwrap(), b"new");
        assert!(!base.join("stale").exists());
        assert!(!scratch.0.join(BASE_TMP_DIR).exists());
        assert!(!scratch.0.join(format!("{BASE_DIR}.old")).exists());
    }

    #[test]
    fn scaffold_never_clobbers_an_existing_mod() {
        let scratch = Scratch::new("scaffold");
        scaffold_mod(&scratch.0).unwrap();
        let json = scratch.0.join(MOD_DIR).join(MOD_JSON);
        fs::write(&json, b"edited").unwrap();

        scaffold_mod(&scratch.0).unwrap();
        assert_eq!(fs::read(&json).unwrap(), b"edited");
    }
}
