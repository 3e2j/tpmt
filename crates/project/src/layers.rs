//! `mod/overlay/` laid over `base/`. Both hold files at the same project
//! paths, and a file comes from the overlay if it's there and from `base/`
//! otherwise.

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::io::fs;
use crate::path::{checked, files};
use crate::store::{Digests, digest_file};
use crate::{Error, Result};

/// One file that differs from vanilla.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    /// A project path, under `mod/overlay/`.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
}

/// Which layer a file was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Overlay,
    Base,
}

/// Every overlay file, split by whether it differs from vanilla. Both halves
/// are sorted by path.
#[derive(Debug, Default)]
pub struct Comparison {
    pub changes: Vec<Change>,
    /// Overlay files byte for byte what the unpack wrote at the same path.
    pub identical: Vec<String>,
}

/// The two layers, by directory. Found with
/// [`Project::layers`](crate::Project::layers).
pub struct Layers {
    base: PathBuf,
    overlay: PathBuf,
}

impl Layers {
    pub(crate) const fn new(base: PathBuf, overlay: PathBuf) -> Self {
        Self { base, overlay }
    }

    /// One file's bytes and the layer they came from, the overlay first.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::MissingFile`] if neither layer holds a file at `path`
    /// - [`Error::Io`] if the file can't be read
    pub fn read(&self, path: &str) -> Result<(Layer, Vec<u8>)> {
        let at = checked(path)?;
        for (layer, dir) in [(Layer::Overlay, &self.overlay), (Layer::Base, &self.base)] {
            if let Some(data) = fs::read_if_exists(&dir.join(at))? {
                return Ok((layer, data));
            }
        }
        Err(Error::MissingFile(path.to_string()))
    }

    /// Writes `data` to `path` in the overlay. `base/` is never written.
    ///
    /// # Errors
    ///
    /// - [`Error::UnusablePath`] if `path` is empty, absolute, or climbs out
    /// - [`Error::Io`] on the write
    pub fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        fs::write(&self.overlay.join(checked(path)?), data)
    }

    /// Whether either layer holds a file at `path`.
    #[must_use]
    pub fn is_file(&self, path: &str) -> bool {
        checked(path)
            .is_ok_and(|at| self.overlay.join(at).is_file() || self.base.join(at).is_file())
    }

    /// Hashes every overlay file against the digests its unpack recorded.
    ///
    /// `base/` is not checked. A build refuses a `base/` file that drifted
    /// when it reads one, and hashing all of `base/` here would cost as much
    /// as the disc is large.
    ///
    /// # Errors
    ///
    /// - [`Error::Io`] if the overlay cannot be walked or a file read
    /// - [`Error::UnusablePath`] if a name in it is not UTF-8
    pub fn compare(&self, digests: &Digests) -> Result<Comparison> {
        let compared = files(&self.overlay)?
            .into_par_iter()
            .map(|path| {
                let kind = change(&self.overlay.join(&path), &path, digests)?;
                Ok((path, kind))
            })
            .collect::<Result<Vec<_>>>()?;

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
    Ok((digest_file(file)? != *want).then_some(ChangeKind::Modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::digest;
    use tempfile::TempDir;

    const PATH: &str = "files/res/a.arc/m.bmg";

    fn layers(scratch: &TempDir) -> Layers {
        Layers::new(scratch.path().join("base"), scratch.path().join("overlay"))
    }

    fn project() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(&layers(&scratch).base.join(PATH), b"vanilla").unwrap();
        scratch
    }

    #[test]
    fn a_read_takes_the_overlay_over_base() {
        let scratch = project();
        let layers = layers(&scratch);
        assert_eq!(
            layers.read(PATH).unwrap(),
            (Layer::Base, b"vanilla".to_vec())
        );

        layers.write(PATH, b"edited").unwrap();
        assert_eq!(
            layers.read(PATH).unwrap(),
            (Layer::Overlay, b"edited".to_vec())
        );
        assert_eq!(fs::read(&layers.base.join(PATH)).unwrap(), b"vanilla");
    }

    #[test]
    fn a_path_in_neither_layer_is_missing() {
        let scratch = project();
        let layers = layers(&scratch);
        assert!(matches!(
            layers.read("files/none.bmg"),
            Err(Error::MissingFile(path)) if path == "files/none.bmg"
        ));
        assert!(matches!(
            layers.read("files/res"),
            Err(Error::MissingFile(_))
        ));
    }

    #[test]
    fn a_path_that_leaves_the_layer_is_refused() {
        let scratch = project();
        let layers = layers(&scratch);
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "files/../../outside",
            "./files",
        ] {
            assert!(
                matches!(layers.read(path), Err(Error::UnusablePath(_))),
                "{path}"
            );
            assert!(
                matches!(layers.write(path, b""), Err(Error::UnusablePath(_))),
                "{path}"
            );
        }
    }

    /// A project whose unpack wrote two files, returning their digests.
    fn unpacked(layers: &Layers) -> Digests {
        let mut digests = Digests::new();
        for (path, data) in [("files/a.bin", b"a"), ("files/b.arc/m.bin", b"m")] {
            fs::write(&layers.base.join(path), data).unwrap();
            digests.insert(path.to_string(), digest(data));
        }
        fs::create_dir_all(&layers.overlay).unwrap();
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
        let layers = layers(&scratch);
        let digests = unpacked(&layers);
        assert_eq!(layers.compare(&digests).unwrap().changes, []);
    }

    #[test]
    fn overlay_edits_and_additions_are_reported() {
        let scratch = tempfile::tempdir().unwrap();
        let layers = layers(&scratch);
        let digests = unpacked(&layers);
        layers.write("files/a.bin", b"edited").unwrap();
        layers.write("files/b.arc/new.bin", b"new").unwrap();

        assert_eq!(
            layers.compare(&digests).unwrap().changes,
            [
                change("files/a.bin", ChangeKind::Modified),
                change("files/b.arc/new.bin", ChangeKind::Added),
            ]
        );
    }

    /// An overlay copy of a vanilla file changes nothing on the disc.
    #[test]
    fn an_overlay_file_identical_to_vanilla_is_not_a_change() {
        let scratch = tempfile::tempdir().unwrap();
        let layers = layers(&scratch);
        let digests = unpacked(&layers);
        layers.write("files/a.bin", b"a").unwrap();

        let comparison = layers.compare(&digests).unwrap();
        assert_eq!(comparison.changes, []);
        assert_eq!(comparison.identical, ["files/a.bin"]);
    }
}
