//! The project as one file tree, with `mod/overlay/` laid over `base/`.
//!
//! Both use the same paths. A file comes from the overlay if it's there, and
//! from `base/` otherwise. Only overlay files that differ from vanilla count
//! as changes, so only the disc files holding them get rebuilt.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tpmt_jkernel_arc::editable::sidecar::{Member, SIDECAR, Sidecar};

use crate::project::metadata::sha1_hex;
use crate::{Error, Result, fs, project, status};

/// The two layers a build reads, in the order it reads them.
pub struct Tree {
    base: PathBuf,
    overlay: PathBuf,
    /// The vanilla sha1 of every file the unpack wrote, keyed by project path.
    hashes: BTreeMap<String, String>,
    /// Every file under `mod/overlay/` that differs from vanilla, as sorted
    /// project paths.
    edits: Vec<String>,
}

impl Tree {
    /// Opens a project's layers, along with the overlay files that are
    /// identical to vanilla.
    ///
    /// Those are left out of the build. They are returned rather than
    /// dropped, because a modder who put a file there meant to change it.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if the overlay cannot be walked or read
    /// - [`Error::UnusablePath`] if a name in it is not UTF-8
    pub fn open(project: &Path, hashes: BTreeMap<String, String>) -> Result<(Self, Vec<String>)> {
        let overlay = project::overlay(project);
        let overlaid = fs::files(&overlay)?;

        let flagged = overlaid
            .into_par_iter()
            .map(|path| Ok((status::diff(&overlay, &path, &hashes)?.is_none(), path)))
            .collect::<Result<Vec<_>>>()?;
        let (identical, edits): (Vec<_>, Vec<_>) = flagged.into_iter().partition(|(same, _)| *same);
        let paths =
            |flagged: Vec<(bool, String)>| flagged.into_iter().map(|(_, path)| path).collect();

        let tree = Self {
            base: project::base(project),
            overlay,
            hashes,
            edits: paths(edits),
        };
        Ok((tree, paths(identical)))
    }

    /// The disc files the overlay changed, each named once.
    ///
    /// A disc holds whole files, so the unit of a rebuild is a whole file
    /// too. Editing one message table inside `files/res/Msgus/bmgres3.arc`
    /// means that archive is written again; editing four of them still means
    /// that archive is written once.
    #[must_use]
    pub fn changed(&self) -> BTreeSet<String> {
        self.edits
            .iter()
            .map(|edit| self.outermost("", edit).to_string())
            .collect()
    }

    /// One project file's bytes, the overlay's copy where there is one.
    ///
    /// A copy out of `base/` is held to its vanilla digest on the way past.
    /// A rebuild reads every unedited member straight out of `base/`, so a
    /// file edited there in place would be packed as though the disc had
    /// shipped it. A path the unpack never wrote fails the same way: either
    /// answer means `base/` is no longer the disc it came from.
    pub fn file(&self, path: &str) -> Result<Vec<u8>> {
        let overlaid = self.overlay.join(path);
        if overlaid.is_file() {
            return fs::read(&overlaid);
        }

        let vanilla = self.base.join(path);
        if vanilla.is_file() {
            let data = fs::read(&vanilla)?;
            if !is_vanilla(&self.hashes, path, &data) {
                return Err(Error::BaseModified(path.to_string()));
            }
            return Ok(data);
        }

        Err(Error::MissingFile(path.to_string()))
    }

    /// Whether a project path is an unpacked archive rather than a leaf file.
    ///
    /// The sidecar is what says so, in either layer. Unpack writes one for
    /// every archive it opens, so a directory without one is a plain
    /// directory, whatever its name ends in: a modder starting an archive of
    /// their own writes the sidecar that describes it, the same way
    /// [`Sidecar::fresh`] would.
    #[must_use]
    pub fn is_archive(&self, path: &str) -> bool {
        self.overlay.join(path).join(SIDECAR).is_file()
            || self.base.join(path).join(SIDECAR).is_file()
    }

    /// What an archive says about itself, the overlay's copy where there is
    /// one, so a modder can move a member into auxiliary memory by editing
    /// the sidecar alone.
    pub fn sidecar(&self, path: &str) -> Result<Sidecar> {
        let at = format!("{path}/{SIDECAR}");
        let data = self.file(&at)?;
        let text = std::str::from_utf8(&data).map_err(fs::parse_at(Path::new(&at)))?;
        Sidecar::from_toml(text).map_err(fs::parse_at(Path::new(&at)))
    }

    /// Every member an archive rebuilds from: the ones its sidecar lists, in
    /// the order it lists them, then anything the overlay added that it does
    /// not mention.
    ///
    /// Added members take main memory and no id, and sort in after the
    /// members already there, by name. The sort is stable and the stored
    /// order is grouped by memory already, so an archive nobody added to
    /// comes back in exactly its own order.
    ///
    /// Only the overlay can add one, since a member `base/` holds and the
    /// sidecar does not mention is `base/` having been edited. A nested
    /// archive is one member rather than a directory of them.
    #[must_use]
    pub fn members(&self, path: &str, sidecar: &Sidecar) -> Vec<Member> {
        let added: BTreeSet<&str> = self
            .edits
            .iter()
            .filter_map(|edit| edit.strip_prefix(path)?.strip_prefix('/'))
            .filter(|rest| *rest != SIDECAR)
            .map(|rest| self.outermost(path, rest))
            .filter(|member| !sidecar.members.iter().any(|held| held.path == *member))
            .collect();

        let mut members = sidecar.members.clone();
        members.extend(
            added
                .into_iter()
                .map(|member| Member::new(member.to_string())),
        );
        members.sort_by_key(|member| member.preload);
        members
    }

    /// The shallowest archive on the way from `under` down to `under/rest`,
    /// relative to `under`, or `rest` itself when nothing on the way is one.
    ///
    /// Shallowest rather than nearest, because a nested archive has no entry
    /// of its own in whatever holds it. Replacing a file two archives deep
    /// means writing the outer one.
    fn outermost<'p>(&self, under: &str, rest: &'p str) -> &'p str {
        rest.match_indices('/')
            .filter_map(|(end, _)| rest.get(..end))
            .find(|prefix| self.is_archive(&fs::join(under, prefix)))
            .unwrap_or(rest)
    }
}

/// Whether `data` is what the unpack wrote at `path`. A path it never wrote
/// has no hash, so it never is.
fn is_vanilla(hashes: &BTreeMap<String, String>, path: &str, data: &[u8]) -> bool {
    hashes.get(path).is_some_and(|want| *want == sha1_hex(data))
}
