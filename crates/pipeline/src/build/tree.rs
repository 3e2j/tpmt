//! The project as one file tree, with `mod/overlay/` laid over `base/`.
//!
//! Both use the same paths. A file comes from the overlay if it's there, and
//! from `base/` otherwise. Only overlay files that differ from vanilla count
//! as changes, so only the disc files holding them get rebuilt.

use std::collections::BTreeSet;
use std::path::Path;

use tpmt_archive::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_project::path::join;
use tpmt_project::store::{Digests, digest};
use tpmt_project::{Comparison, Layer, Layers, Project};

use crate::{Error, Result};

/// The two layers a build reads, in the order it reads them.
pub struct Tree {
    layers: Layers,
    /// The vanilla digest of every file the unpack wrote, keyed by project path.
    digests: Digests,
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
    /// - [`tpmt_project::Error::Io`] if the overlay cannot be walked or read
    /// - [`tpmt_project::Error::UnusablePath`] if a name in it is not UTF-8
    pub fn open(project: &Project, digests: Digests) -> Result<(Self, Vec<String>)> {
        let layers = project.layers();
        let Comparison { changes, identical } = layers.compare(&digests)?;

        let tree = Self {
            layers,
            digests,
            edits: changes.into_iter().map(|change| change.path).collect(),
        };
        Ok((tree, identical))
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
        let (layer, data) = self.layers.read(path)?;
        if layer == Layer::Base && !is_vanilla(&self.digests, path, &data) {
            return Err(Error::BaseModified(path.to_string()));
        }
        Ok(data)
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
        self.layers.is_file(&join(path, SIDECAR))
    }

    /// What an archive says about itself, the overlay's copy where there is
    /// one, so a modder can move a member into auxiliary memory by editing
    /// the sidecar alone.
    pub fn sidecar(&self, path: &str) -> Result<Sidecar> {
        let at = format!("{path}/{SIDECAR}");
        let data = self.file(&at)?;
        let text =
            std::str::from_utf8(&data).map_err(tpmt_project::Error::parse(Path::new(&at)))?;
        Ok(Sidecar::from_toml(text).map_err(tpmt_project::Error::parse(Path::new(&at)))?)
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
            .find(|prefix| self.is_archive(&join(under, prefix)))
            .unwrap_or(rest)
    }
}

/// Whether `data` is what the unpack wrote at `path`. A path it never wrote
/// has no digest, so it never is.
fn is_vanilla(digests: &Digests, path: &str, data: &[u8]) -> bool {
    digests.get(path).is_some_and(|want| *want == digest(data))
}
