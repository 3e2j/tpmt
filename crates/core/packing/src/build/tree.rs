//! The project as one file tree, read through [`Files`], with the changes
//! that make it differ from vanilla.
//!
//! Only the disc files holding a change get rebuilt.

use std::collections::BTreeSet;

use tpmt_archive::editable::sidecar::{Member, SIDECAR, Sidecar};

use super::Files;
use crate::Error;

/// Every project file, and which ones changed.
pub struct Tree<'a, E> {
    files: &'a dyn Files<E>,
    /// Every project file that differs from vanilla, sorted.
    changes: &'a [String],
}

impl<'a, E: From<Error>> Tree<'a, E> {
    pub fn new(files: &'a dyn Files<E>, changes: &'a [String]) -> Self {
        Self { files, changes }
    }

    /// The disc files the changes touch, each named once.
    ///
    /// A disc holds whole files, so the unit of a rebuild is a whole file
    /// too. Editing one message table inside `files/res/Msgus/bmgres3.arc`
    /// means that archive is written again; editing four of them still means
    /// that archive is written once.
    #[must_use]
    pub fn rebuilt(&self) -> BTreeSet<String> {
        self.changes
            .iter()
            .map(|change| self.outermost("", change).to_string())
            .collect()
    }

    /// One project file's bytes, the edited copy where there is one.
    pub fn file(&self, path: &str) -> Result<Box<[u8]>, E> {
        self.files.read(path)
    }

    /// Whether a project path is an unpacked archive rather than a plain file.
    ///
    /// The sidecar is what says so, edited or not. Unpack writes one for
    /// every archive it opens, so a directory without one is a plain
    /// directory, whatever its name ends in: a modder starting an archive of
    /// their own writes the sidecar that describes it, the same way
    /// [`Sidecar::fresh`] would.
    #[must_use]
    pub fn is_archive(&self, path: &str) -> bool {
        self.files.is_file(&join(path, SIDECAR))
    }

    /// What an archive says about itself, the edited copy where there is
    /// one, so a modder can move a member into auxiliary memory by editing
    /// the sidecar alone.
    pub fn sidecar(&self, path: &str) -> Result<Sidecar, E> {
        let at = format!("{path}/{SIDECAR}");
        let data = self.file(&at)?;
        let parsed = std::str::from_utf8(&data)
            .map_err(Into::into)
            .and_then(|text| Sidecar::from_toml(text).map_err(Into::into));
        Ok(parsed.map_err(|source| Error::Sidecar { path: at, source })?)
    }

    /// Every member an archive rebuilds from: the ones its sidecar lists, in
    /// the order it lists them, then anything a change added that it does
    /// not mention.
    ///
    /// Added members take main memory and no id, and sort in after the
    /// members already there, by name. The sort is stable and the stored
    /// order is grouped by memory already, so an archive nobody added to
    /// comes back in exactly its own order.
    ///
    /// Only a change can add one, since a vanilla member the sidecar does not
    /// mention means the vanilla copy itself was changed. A nested archive is
    /// one member rather than a directory of them.
    #[must_use]
    pub fn members(&self, path: &str, sidecar: &Sidecar) -> Vec<Member> {
        let added: BTreeSet<&str> = self
            .changes
            .iter()
            .filter_map(|change| change.strip_prefix(path)?.strip_prefix('/'))
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

/// `path` under the project path `under`, which may be the root.
fn join(under: &str, path: &str) -> String {
    if under.is_empty() {
        path.to_string()
    } else {
        format!("{under}/{path}")
    }
}
