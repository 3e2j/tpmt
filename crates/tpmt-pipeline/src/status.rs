//! Comparing `mod/overlay/` against the hashes its unpack recorded.
//!
//! `base/` is not checked. A build refuses a `base/` file that drifted when
//! it reads one, and hashing all of `base/` here would cost as much as the
//! disc is large.

use std::collections::BTreeMap;
use std::path::Path;

use rayon::prelude::*;

use crate::project::metadata::{self, sha1_file};
use crate::{Change, ChangeKind, Result, fs, project};

pub fn run(project: &Path) -> Result<Vec<Change>> {
    let metadata::Store { hashes, .. } = metadata::read_store(project)?;
    let overlay = project::overlay(project);
    fs::files(&overlay)?
        .into_par_iter()
        .map(|path| Ok(diff(&overlay, &path, &hashes)?.map(|kind| Change { path, kind })))
        .filter_map(Result::transpose)
        .collect()
}

/// What the file at `dir/path` is next to what the unpack wrote at `path`,
/// or `None` if it is the same bytes.
pub fn diff(
    dir: &Path,
    path: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<Option<ChangeKind>> {
    let Some(want) = hashes.get(path) else {
        return Ok(Some(ChangeKind::Added));
    };
    Ok((sha1_file(&dir.join(path))? != *want).then_some(ChangeKind::Modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::metadata::sha1_hex;
    use crate::test_support::Scratch;

    /// A project whose unpack wrote two files.
    fn unpacked(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        let base = project::base(&scratch.0);

        let mut hashes = BTreeMap::new();
        for (path, data) in [("files/a.bin", b"a"), ("files/b.arc/m.bin", b"m")] {
            fs::write(&base.join(path), data).unwrap();
            hashes.insert(path.to_string(), sha1_hex(data));
        }
        fs::create_dir_all(&project::overlay(&scratch.0)).unwrap();

        let iso = scratch.0.join("source.iso");
        fs::write(&iso, b"").unwrap();
        metadata::write_store(&scratch.0, &iso, "0", &hashes, &metadata::Formats::new()).unwrap();
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
        let scratch = unpacked("fresh");
        assert_eq!(run(&scratch.0).unwrap(), []);
    }

    #[test]
    fn overlay_edits_and_additions_are_reported() {
        let scratch = unpacked("overlay");
        let overlay = project::overlay(&scratch.0);
        fs::write(&overlay.join("files/a.bin"), b"edited").unwrap();
        fs::write(&overlay.join("files/b.arc/new.bin"), b"new").unwrap();

        assert_eq!(
            run(&scratch.0).unwrap(),
            [
                change("files/a.bin", ChangeKind::Modified),
                change("files/b.arc/new.bin", ChangeKind::Added),
            ]
        );
    }

    /// An overlay copy of a vanilla file changes nothing on the disc.
    #[test]
    fn an_overlay_file_identical_to_vanilla_is_not_a_change() {
        let scratch = unpacked("identical");
        fs::write(&project::overlay(&scratch.0).join("files/a.bin"), b"a").unwrap();

        assert_eq!(run(&scratch.0).unwrap(), []);
    }
}
