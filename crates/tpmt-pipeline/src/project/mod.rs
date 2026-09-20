//! The project directory: where everything in it lives, how to find one,
//! and how its directories come and go. The files tpmt writes into it are
//! [`metadata`]'s.
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
//! Every unpack rewrites `base/` whole. [`scaffold_mod`] writes `mod/` once.
//!
//! Facts a decoded file can't carry, like a wrapper that came off it or
//! which memory an archive member loads into, live in a sidecar next to it
//! instead, one name per format:
//!
//! ```text
//! *.arc/.tpmt-arc.toml   what an unpacked archive is, minus its bytes
//! ```
//!
//! Everything generated about the project, rather than for it, lives in
//! `.tpmt/` (see [`metadata`]). It goes in last, after everything else
//! succeeded, so its presence means an unpack finished, which is what
//! [`is_project`] tests.
// # TODO
// One disc per project FOR NOW.
//
// A modder may bring more than one region (GZ2E, GZ2P, GZ2J), in which case
// the first unpacked (by the modder) is the primary copy and each other region's
// files show only where their hash differs from the primary's file at the same path.
// That needs `hashes.toml` and `source.toml` keyed per region; both hold
// one disc today.
// Nothing here decides which regions an edit applies to yet.

use std::fs;
use std::path::{Path, PathBuf};

use crate::fs::{create_dir_all, io_at, remove_dir_all_if_exists};
use crate::{Error, Result};

pub mod metadata;

use metadata::ModMetadata;

// Base game
/// Read-only unpack of the game.
pub const BASE_DIR: &str = "base";
/// Where [`Staging`] writes a fresh unpack before promoting it to [`BASE_DIR`].
const BASE_TMP_DIR: &str = "base.tmp";
/// Where the previous [`BASE_DIR`] sits during a promote.
const BASE_OLD_DIR: &str = "base.old";
/// The disc preamble values a build cannot derive, under [`BASE_DIR`].
const DISC_TOML: &str = "disc.toml";
/// Which loose files arrived Yaz0 wrapped, under [`BASE_DIR`].
const YAZ0_TOML: &str = "yaz0.toml";

// Mod (authored) directory
/// The mod project: `overlay/`, `res/`, `mod.json`.
pub const MOD_DIR: &str = "mod";
const OVERLAY_DIR: &str = "overlay";
const RES_DIR: &str = "res";
const SCRIPTS_DIR: &str = "scripts";
const MOD_JSON: &str = "mod.json";

// Build output
const BUILD_DIR: &str = "build";

// TPMT specifics
const STORE_DIR: &str = ".tpmt";
const HASHES_TOML: &str = "hashes.toml";
const SOURCE_TOML: &str = "source.toml";

/// Every top-level name this crate writes. A directory holding nothing but
/// these is ours, however far an unpack got before it failed.
const OWNED: [&str; 6] = [
    BASE_DIR,
    BASE_TMP_DIR,
    BASE_OLD_DIR,
    MOD_DIR,
    BUILD_DIR,
    STORE_DIR,
];

/// Whether `dir` is a finished unpack: it has the `.tpmt/` that only
/// `metadata::write_store` writes, and only after everything else is in
/// place.
#[must_use]
pub fn is_project(dir: &Path) -> bool {
    dir.join(STORE_DIR).is_dir()
}

/// Finds the project root by walking upward from `start`. Canonicalizes
/// first, then climbs one directory at a time until a `.tpmt` store turns
/// up or the climb hits the filesystem root.
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

/// Refuses a directory that is not a project but already holds files.
///
/// A project passes whatever else it holds (notes, fixtures, `.git`), since
/// a re-unpack replaces only `base/`. An empty or missing directory passes,
/// as does one holding only [`OWNED`] names from an unpack that failed part
/// way.
pub fn refuse_foreign(project: &Path) -> Result<()> {
    if is_project(project) || !project.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(project).map_err(io_at(project))? {
        let entry = entry.map_err(io_at(project))?;
        if !OWNED.iter().any(|name| entry.file_name() == *name) {
            return Err(Error::ForeignDirectory(project.to_path_buf()));
        }
    }
    Ok(())
}

/// A fresh `base/` being written under a temporary name, swapped in by
/// [`promote`](Self::promote) once complete and thrown away otherwise.
///
/// Dropping one unpromoted removes whatever it wrote, so a failed unpack
/// leaves the project as it found it.
pub struct Staging {
    project: PathBuf,
    dir: PathBuf,
}

impl Staging {
    /// Clears anything a previous failed unpack left behind and opens a
    /// fresh staging directory.
    pub fn begin(project: &Path) -> Result<Self> {
        let dir = project.join(BASE_TMP_DIR);
        remove_dir_all_if_exists(&dir)?;
        create_dir_all(&dir)?;
        Ok(Self {
            project: project.to_path_buf(),
            dir,
        })
    }

    /// Where the unpack writes `base/`'s contents.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Swaps the staged tree in as `base/`. Moves the old `base/` aside
    /// rather than deleting it first, so a failure between the two renames
    /// leaves it recoverable as [`BASE_OLD_DIR`].
    pub fn promote(self) -> Result<()> {
        let base = self.project.join(BASE_DIR);
        let old = self.project.join(BASE_OLD_DIR);

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
        // Best effort. After a promote there is nothing here, and after a
        // failure the error that caused it matters more.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Writes the `mod/` skeleton (`overlay/`, `res/scripts/`, a starter
/// `mod.json`) alongside `base/`. Skips an existing `mod/`, so re-unpacking
/// a project never clobbers a modder's edits.
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
    metadata::write_mod(
        &mod_dir,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::fs::write;

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

    /// The workspace disallows `fs::read` because an ISO may not fit in
    /// memory. A test file does.
    fn read(path: &Path) -> Vec<u8> {
        use std::io::Read;
        let mut data = Vec::new();
        fs::File::open(path)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        data
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

    /// Unpacking into a directory the user already keeps their own files in
    /// fails rather than clearing it to make room.
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

    /// An unpack that died before the store went in leaves only names this
    /// crate wrote, so the next attempt can carry on rather than refuse.
    #[test]
    fn accepts_a_half_finished_unpack() {
        let scratch = Scratch::new("half");
        write(&scratch.0.join(BASE_DIR).join("files").join("a"), b"").unwrap();
        write(&scratch.0.join(BASE_TMP_DIR).join("files").join("a"), b"").unwrap();
        fs::create_dir_all(scratch.0.join(MOD_DIR)).unwrap();

        refuse_foreign(&scratch.0).unwrap();
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

        assert_eq!(read(&base.join("fresh")), b"new");
        assert!(!base.join("stale").exists());
        assert!(!scratch.0.join(BASE_TMP_DIR).exists());
        assert!(!scratch.0.join(BASE_OLD_DIR).exists());
    }

    #[test]
    fn scaffold_never_clobbers_an_existing_mod() {
        let scratch = Scratch::new("scaffold");
        scaffold_mod(&scratch.0).unwrap();
        let json = scratch.0.join(MOD_DIR).join(MOD_JSON);
        fs::write(&json, b"edited").unwrap();

        scaffold_mod(&scratch.0).unwrap();
        assert_eq!(read(&json), b"edited");
    }
}
