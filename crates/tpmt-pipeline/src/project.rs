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
//! .tpmt/hashes        vanilla hashes of base/, for change detection
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
/// Staging area for a fresh unpack, promoted to [`BASE_DIR`] once complete.
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

// Build directory
/// Built mods and images. Absent until a build makes one.
pub const BUILD_DIR: &str = "build";

// TPMT specifics
const STORE_DIR: &str = ".tpmt";
const HASHES: &str = "hashes";
const SOURCE: &str = "source.toml";

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

/// Makes `project` somewhere an unpack may write into. `base/` is left alone
/// until [`promote_base`] replaces it; only a stale `base.tmp` from a
/// previous failed unpack is cleared. A directory holding anything else is
/// refused.
pub fn prepare(project: &Path) -> Result<()> {
    if is_project(project) {
        return clear_staging(project);
    }
    if project.is_dir()
        && fs::read_dir(project)
            .map_err(io_at(project))?
            .next()
            .is_some()
    {
        return Err(Error::ForeignDirectory(project.to_path_buf()));
    }
    Ok(())
}

/// Where a fresh unpack should write `base/`'s contents.
#[must_use]
pub fn base_staging_dir(project: &Path) -> PathBuf {
    project.join(BASE_TMP_DIR)
}

/// Discards an in-progress unpack's staging directory, e.g. after it fails.
pub fn clear_staging(project: &Path) -> Result<()> {
    remove_dir_all_if_exists(&project.join(BASE_TMP_DIR))
}

/// Swaps a finished [`base_staging_dir`] in as `base/`. Only called once the
/// new unpack is fully written.
pub fn promote_base(project: &Path) -> Result<()> {
    let base = project.join(BASE_DIR);
    remove_dir_all_if_exists(&base)?;
    fs::rename(project.join(BASE_TMP_DIR), &base).map_err(io_at(&base))
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
    write_toml(&store.join(HASHES), hashes)?;
    write_toml(&store.join(SOURCE), &Source { iso: &iso, sha1 })
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
    write(path, toml::to_string_pretty(value)?.as_bytes())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write(path, serde_json::to_string_pretty(value)?.as_bytes())
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
