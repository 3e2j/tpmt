//! Comparing `mod/overlay/` against the digests its unpack recorded.
//!
//! `base/` is not checked. A build refuses a `base/` file that drifted when
//! it reads one, and hashing all of `base/` here would cost as much as the
//! disc is large.

use std::path::Path;

use rayon::prelude::*;

use crate::metadata::{self, Digests, digest_file};
use crate::{Change, ChangeKind, Result, fs, layout};

pub(crate) fn run(project: &Path) -> Result<Vec<Change>> {
    let metadata::Store { digests, .. } = metadata::read_store(project)?;
    let overlay = layout::overlay(project);
    fs::files(&overlay)?
        .into_par_iter()
        .map(|path| Ok(file(&overlay, &path, &digests)?.map(|kind| Change { path, kind })))
        .filter_map(Result::transpose)
        .collect()
}

/// What the file at `dir/path` is next to what the unpack wrote at `path`,
/// or `None` if it is the same bytes.
///
/// # Errors
///
/// - [`Error::Io`](crate::Error::Io) if the file cannot be read
pub fn file(dir: &Path, path: &str, digests: &Digests) -> Result<Option<ChangeKind>> {
    let Some(want) = digests.get(path) else {
        return Ok(Some(ChangeKind::Added));
    };
    Ok((digest_file(&dir.join(path))? != *want).then_some(ChangeKind::Modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::digest;
    use tempfile::TempDir;
    use tpmt_disc::Boot;

    /// A project whose unpack wrote two files.
    fn unpacked() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        let base = layout::base(scratch.path());

        let mut digests = Digests::new();
        for (path, data) in [("files/a.bin", b"a"), ("files/b.arc/m.bin", b"m")] {
            fs::write(&base.join(path), data).unwrap();
            digests.insert(path.to_string(), digest(data));
        }
        fs::create_dir_all(&layout::overlay(scratch.path())).unwrap();

        let iso = scratch.path().join("source.iso");
        fs::write(&iso, b"").unwrap();
        metadata::write_store(
            scratch.path(),
            &iso,
            &Boot {
                id: "GZ2E".to_string(),
                maker: "01".to_string(),
                disc_number: 0,
                revision: 0,
                audio_streaming: 0,
                stream_buffer_size: 0,
                title: "test".to_string(),
            },
            &digests,
            &metadata::Formats::new(),
        )
        .unwrap();
        scratch
    }

    fn change(path: &str, kind: ChangeKind) -> Change {
        Change {
            path: path.to_string(),
            kind,
        }
    }

    #[test]
    fn a_fresh_unpack_has_no_changes() {
        let scratch = unpacked();
        assert_eq!(run(scratch.path()).unwrap(), []);
    }

    #[test]
    fn overlay_edits_and_additions_are_reported() {
        let scratch = unpacked();
        let overlay = layout::overlay(scratch.path());
        fs::write(&overlay.join("files/a.bin"), b"edited").unwrap();
        fs::write(&overlay.join("files/b.arc/new.bin"), b"new").unwrap();

        assert_eq!(
            run(scratch.path()).unwrap(),
            [
                change("files/a.bin", ChangeKind::Modified),
                change("files/b.arc/new.bin", ChangeKind::Added),
            ]
        );
    }

    /// An overlay copy of a vanilla file changes nothing on the disc.
    #[test]
    fn an_overlay_file_identical_to_vanilla_is_not_a_change() {
        let scratch = unpacked();
        fs::write(&layout::overlay(scratch.path()).join("files/a.bin"), b"a").unwrap();

        assert_eq!(run(scratch.path()).unwrap(), []);
    }
}
