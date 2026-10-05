//! Reading and writing a project's game files, and listing ones that differ
//! from vanilla.
//!
//! The files live in two directories:
//! `vanilla/` holds what the unpack wrote (read-only),
//! `mod/changes/` holds the user's changes.
//!
//! A file in `changes/` is used as a replacement, or a patch against its `vanilla/` origin.
//! Patches are stored at the same origin path plus [`PATCH_SUFFIX`].
//!
//! This crate stores patches without reading them. `tpmt-ops` applies them.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::io::fs;
use crate::layout::store::{Digests, digest, digest_file};
use crate::path::{checked, files};
use crate::{Error, Result};

/// The patches filetype
pub const PATCH_SUFFIX: &str = ".toml";

/// The path of the patch for the file at `path`.
#[must_use]
pub fn patch_path(path: &str) -> String {
    format!("{path}{PATCH_SUFFIX}")
}

/// One file that differs from vanilla.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    /// A project path: the file in `mod/changes/`, or the file its patch
    /// there patches.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Replaced,
    /// Patched. Never checked against vanilla, since that means putting the
    /// patch over it.
    Patched,
}

/// The bytes the overlay holds for one path.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    /// Only `changes/` has the file.
    Added(Box<[u8]>),
    /// `changes/` holds a whole file in place of the `vanilla/` copy.
    // Holds no vanilla copy until the app wants to show one beside a whole replacement.
    Replaced(Box<[u8]>),
    /// `changes/` holds a patch against the `vanilla/` copy.
    Patched {
        vanilla: Box<[u8]>,
        edits: Box<[u8]>,
    },
    /// Only `vanilla/` has the file.
    Vanilla(Box<[u8]>),
}

/// Every file in `changes/`, split by whether it differs from vanilla. Both
/// halves are sorted by path.
#[derive(Debug, Default)]
pub struct Comparison {
    pub changes: Vec<Change>,
    /// Files in `changes/` byte for byte what the unpack wrote at the same path.
    pub identical: Vec<String>,
}

/// `changes/` read over `vanilla/`. Found with
/// [`Project::overlay`](crate::Project::overlay).
pub struct Overlay {
    vanilla: PathBuf,
    changes: PathBuf,
}

impl Overlay {
    pub(crate) const fn new(vanilla: PathBuf, changes: PathBuf) -> Self {
        Self { vanilla, changes }
    }

    /// What the overlay holds for `path`. A patch counts only over a `vanilla/`
    /// copy; without one, it is a whole file at its own path.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::MissingFile`] if neither directory holds a file at `path`
    /// - [`Error::PatchConflict`] if `changes/` holds it whole and as a patch
    /// - [`Error::Io`] if a file can't be read
    pub fn read(&self, path: &str) -> Result<Stored> {
        let at = checked(path)?;
        let whole = fs::read_if_exists(&self.changes.join(at))?;
        let vanilla = self.vanilla.join(at);
        if !vanilla.is_file() {
            return whole
                .map(|file| Stored::Added(file.into()))
                .ok_or_else(|| Error::MissingFile(path.to_string()));
        }
        let edits = fs::read_if_exists(&self.changes.join(patch_path(path)))?;
        match (whole, edits) {
            (Some(_), Some(_)) => Err(Error::PatchConflict(path.to_string())),
            (Some(file), None) => Ok(Stored::Replaced(file.into())),
            (None, Some(edits)) => Ok(Stored::Patched {
                vanilla: fs::read(&vanilla)?.into(),
                edits: edits.into(),
            }),
            (None, None) => Ok(Stored::Vanilla(fs::read(&vanilla)?.into())),
        }
    }

    /// [`read`](Self::read), holding a `vanilla/` copy to the digest the
    /// unpack recorded for it, patched or not.
    ///
    /// A rebuild reads every unedited member straight out of `vanilla/`, so a
    /// file edited there in place would be packed as though the disc had
    /// shipped it. A path the unpack never wrote fails the same way: either
    /// answer means `vanilla/` is no longer the disc it came from.
    ///
    /// # Errors
    ///
    /// - [`Error::VanillaModified`] if the `vanilla/` copy doesn't match its digest
    /// - as [`read`](Self::read)
    pub fn read_checked(&self, path: &str, digests: &Digests) -> Result<Stored> {
        let stored = self.read(path)?;
        let vanilla = match &stored {
            Stored::Added(_) | Stored::Replaced(_) => return Ok(stored),
            Stored::Patched { vanilla, .. } | Stored::Vanilla(vanilla) => vanilla,
        };
        if digests.get(path) != Some(&digest(vanilla)) {
            return Err(Error::VanillaModified(path.to_string()));
        }
        Ok(stored)
    }

    /// Writes `data` to `path` in `changes/` whole. `vanilla/` is never written.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::PatchConflict`] if `changes/` holds a patch for `path`
    /// - [`Error::Io`] on the write
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let at = checked(path)?;
        if self.vanilla.join(at).is_file() && self.changes.join(patch_path(path)).is_file() {
            return Err(Error::PatchConflict(path.to_string()));
        }
        fs::write(&self.changes.join(at), data)
    }

    /// Writes `edits` to `changes/` as the patch for the `vanilla/` copy of
    /// `path`.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::MissingFile`] if `vanilla/` holds no file at `path`
    /// - [`Error::PatchConflict`] if `changes/` holds `path` whole
    /// - [`Error::Io`] on the write
    pub fn write_patch(&self, path: &str, edits: &[u8]) -> Result<()> {
        let at = checked(path)?;
        if !self.vanilla.join(at).is_file() {
            return Err(Error::MissingFile(path.to_string()));
        }
        if self.changes.join(at).is_file() {
            return Err(Error::PatchConflict(path.to_string()));
        }
        fs::write(&self.changes.join(patch_path(path)), edits)
    }

    /// Removes the patch for `path` from `changes/`, if there is one.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::Io`] if the patch exists and can't be removed
    pub fn remove_patch(&self, path: &str) -> Result<()> {
        checked(path)?;
        fs::remove_file_if_exists(&self.changes.join(patch_path(path)))
    }

    /// Whether either directory holds a file at `path`. A patch holds one only
    /// over a `vanilla/` copy, so it adds nothing here.
    #[must_use]
    pub fn is_file(&self, path: &str) -> bool {
        checked(path)
            .is_ok_and(|at| self.changes.join(at).is_file() || self.vanilla.join(at).is_file())
    }

    /// Hashes every file in `changes/` against the digests the unpack
    /// recorded. A patch is a change to the vanilla file it names, and a
    /// file named like one with no vanilla file to patch is an addition.
    ///
    /// `vanilla/` is not checked. A build refuses a `vanilla/` file that drifted
    /// when it reads one, and hashing all of `vanilla/` here would cost as much
    /// as the disc is large.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if `changes/` cannot be walked or a file read
    /// - [`Error::UnusablePath`] if a name in it is not UTF-8
    /// - [`Error::PatchConflict`] if it holds a file whole and as a patch
    pub fn compare(&self, digests: &Digests) -> Result<Comparison> {
        let mut compared = files(&self.changes)?
            .into_par_iter()
            .map(|path| {
                let target = path.strip_suffix(PATCH_SUFFIX);
                if let Some(target) = target.filter(|target| digests.contains_key(*target)) {
                    if self.changes.join(target).is_file() {
                        return Err(Error::PatchConflict(target.to_string()));
                    }
                    return Ok((target.to_string(), Some(ChangeKind::Patched)));
                }
                let kind = change(&self.changes.join(&path), &path, digests)?;
                Ok((path, kind))
            })
            .collect::<Result<Vec<_>>>()?;
        // A patch sorts by the file it patches, which drops its suffix.
        compared.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut comparison = Comparison::default();
        for (path, kind) in compared {
            match kind {
                Some(kind) => comparison.changes.push(Change { path, kind }),
                None => comparison.identical.push(path),
            }
        }
        Ok(comparison)
    }
}

/// What the file at `file` is next to what the unpack wrote at `path`, or
/// `None` if it is the same bytes.
fn change(file: &Path, path: &str, digests: &Digests) -> Result<Option<ChangeKind>> {
    let Some(want) = digests.get(path) else {
        return Ok(Some(ChangeKind::Added));
    };
    Ok((digest_file(file)? != *want).then_some(ChangeKind::Replaced))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::store::digest;
    use tempfile::TempDir;

    const PATH: &str = "files/res/a.arc/m.bmg";

    fn overlay(scratch: &TempDir) -> Overlay {
        Overlay::new(
            scratch.path().join("vanilla"),
            scratch.path().join("changes"),
        )
    }

    fn project() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(&overlay(&scratch).vanilla.join(PATH), b"vanilla").unwrap();
        scratch
    }

    #[test]
    fn a_read_takes_changes_over_vanilla() {
        let scratch = project();
        let overlay = overlay(&scratch);
        assert_eq!(
            overlay.read(PATH).unwrap(),
            Stored::Vanilla(b"vanilla".as_slice().into())
        );

        overlay.write(PATH, b"edited").unwrap();
        assert_eq!(
            overlay.read(PATH).unwrap(),
            Stored::Replaced(b"edited".as_slice().into())
        );
        assert_eq!(fs::read(&overlay.vanilla.join(PATH)).unwrap(), b"vanilla");
    }

    #[test]
    fn a_file_only_in_changes_is_added() {
        let scratch = project();
        let overlay = overlay(&scratch);
        overlay.write("files/new.bmg", b"new").unwrap();
        assert_eq!(
            overlay.read("files/new.bmg").unwrap(),
            Stored::Added(b"new".as_slice().into())
        );
    }

    #[test]
    fn a_path_in_neither_directory_is_missing() {
        let scratch = project();
        let overlay = overlay(&scratch);
        assert!(matches!(
            overlay.read("files/none.bmg"),
            Err(Error::MissingFile(path)) if path == "files/none.bmg"
        ));
        assert!(matches!(
            overlay.read("files/res"),
            Err(Error::MissingFile(_))
        ));
    }

    #[test]
    fn a_path_that_leaves_its_directory_is_refused() {
        let scratch = project();
        let overlay = overlay(&scratch);
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "files/../../outside",
            "./files",
        ] {
            assert!(
                matches!(overlay.read(path), Err(Error::UnusablePath(_))),
                "{path}"
            );
            assert!(
                matches!(overlay.write(path, b""), Err(Error::UnusablePath(_))),
                "{path}"
            );
        }
    }

    #[test]
    fn a_read_hands_back_a_patch_with_its_vanilla_copy() {
        let scratch = project();
        let overlay = overlay(&scratch);
        overlay.write_patch(PATH, b"edits").unwrap();
        assert_eq!(
            overlay.read(PATH).unwrap(),
            Stored::Patched {
                vanilla: b"vanilla".as_slice().into(),
                edits: b"edits".as_slice().into(),
            }
        );

        overlay.remove_patch(PATH).unwrap();
        assert_eq!(
            overlay.read(PATH).unwrap(),
            Stored::Vanilla(b"vanilla".as_slice().into())
        );
    }

    #[test]
    fn a_file_is_never_stored_whole_and_as_a_patch() {
        let scratch = project();
        let overlay = overlay(&scratch);
        overlay.write_patch(PATH, b"edits").unwrap();
        assert!(matches!(
            overlay.write(PATH, b"whole"),
            Err(Error::PatchConflict(_))
        ));

        overlay.remove_patch(PATH).unwrap();
        overlay.write(PATH, b"whole").unwrap();
        assert!(matches!(
            overlay.write_patch(PATH, b"edits"),
            Err(Error::PatchConflict(_))
        ));
    }

    /// A patch needs a vanilla copy to go over.
    #[test]
    fn a_patch_without_a_vanilla_copy_is_refused() {
        let scratch = project();
        assert!(matches!(
            overlay(&scratch).write_patch("files/new.bmg", b"edits"),
            Err(Error::MissingFile(_))
        ));
    }

    /// A project whose unpack wrote two files, returning their digests.
    fn unpacked(overlay: &Overlay) -> Digests {
        let mut digests = Digests::new();
        for (path, data) in [("files/a.bin", b"a"), ("files/b.arc/m.bin", b"m")] {
            fs::write(&overlay.vanilla.join(path), data).unwrap();
            digests.insert(path.to_string(), digest(data));
        }
        fs::create_dir_all(&overlay.changes).unwrap();
        digests
    }

    fn change(path: &str, kind: ChangeKind) -> Change {
        Change {
            path: path.to_string(),
            kind,
        }
    }

    #[test]
    fn a_fresh_unpack_has_no_changes() {
        let scratch = tempfile::tempdir().unwrap();
        let overlay = overlay(&scratch);
        let digests = unpacked(&overlay);
        assert_eq!(overlay.compare(&digests).unwrap().changes, []);
    }

    #[test]
    fn edits_and_additions_are_reported() {
        let scratch = tempfile::tempdir().unwrap();
        let overlay = overlay(&scratch);
        let digests = unpacked(&overlay);
        overlay.write("files/a.bin", b"edited").unwrap();
        overlay.write("files/b.arc/new.bin", b"new").unwrap();

        assert_eq!(
            overlay.compare(&digests).unwrap().changes,
            [
                change("files/a.bin", ChangeKind::Replaced),
                change("files/b.arc/new.bin", ChangeKind::Added),
            ]
        );
    }

    #[test]
    fn a_patch_is_a_change_to_the_file_it_names() {
        let scratch = tempfile::tempdir().unwrap();
        let overlay = overlay(&scratch);
        let digests = unpacked(&overlay);
        overlay.write_patch("files/a.bin", b"").unwrap();
        overlay.write("files/new.toml", b"").unwrap();

        assert_eq!(
            overlay.compare(&digests).unwrap().changes,
            [
                change("files/a.bin", ChangeKind::Patched),
                change("files/new.toml", ChangeKind::Added),
            ]
        );
    }

    /// A copy of a vanilla file in `changes/` changes nothing on the disc.
    #[test]
    fn a_file_identical_to_vanilla_is_not_a_change() {
        let scratch = tempfile::tempdir().unwrap();
        let overlay = overlay(&scratch);
        let digests = unpacked(&overlay);
        overlay.write("files/a.bin", b"a").unwrap();

        let comparison = overlay.compare(&digests).unwrap();
        assert_eq!(comparison.changes, []);
        assert_eq!(comparison.identical, ["files/a.bin"]);
    }
}
