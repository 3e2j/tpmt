//! One leaf file's bytes in and out of a project, with `mod/overlay/` laid
//! over `base/` the same way a build reads them.

use std::path::{Component, Path};

use crate::{Error, Result, fs, layout};

pub fn read(project: &Path, path: &str) -> Result<Vec<u8>> {
    let at = checked(path)?;
    for layer in [layout::overlay(project), layout::base(project)] {
        if let Some(data) = fs::read_if_exists(&layer.join(at))? {
            return Ok(data);
        }
    }
    Err(Error::MissingFile(path.to_string()))
}

pub fn write(project: &Path, path: &str, data: &[u8]) -> Result<()> {
    fs::write(&layout::overlay(project).join(checked(path)?), data)
}

/// `path` as a relative path that stays inside the layer it's joined to.
fn checked(path: &str) -> Result<&Path> {
    let at = Path::new(path);
    let inside = !path.is_empty()
        && at
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if inside {
        Ok(at)
    } else {
        Err(Error::UnusablePath(at.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const PATH: &str = "files/res/a.arc/m.bmg";

    fn project() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        fs::write(&layout::base(scratch.path()).join(PATH), b"vanilla").unwrap();
        scratch
    }

    #[test]
    fn a_read_takes_the_overlay_over_base() {
        let scratch = project();
        assert_eq!(read(scratch.path(), PATH).unwrap(), b"vanilla");

        write(scratch.path(), PATH, b"edited").unwrap();
        assert_eq!(read(scratch.path(), PATH).unwrap(), b"edited");
        assert_eq!(
            fs::read(&layout::base(scratch.path()).join(PATH)).unwrap(),
            b"vanilla"
        );
    }

    #[test]
    fn a_path_in_neither_layer_is_missing() {
        let scratch = project();
        assert!(matches!(
            read(scratch.path(), "files/none.bmg"),
            Err(Error::MissingFile(path)) if path == "files/none.bmg"
        ));
        assert!(matches!(
            read(scratch.path(), "files/res"),
            Err(Error::MissingFile(_))
        ));
    }

    #[test]
    fn a_path_that_leaves_the_layer_is_refused() {
        let scratch = project();
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "files/../../outside",
            "./files",
        ] {
            assert!(
                matches!(read(scratch.path(), path), Err(Error::UnusablePath(_))),
                "{path}"
            );
            assert!(
                matches!(
                    write(scratch.path(), path, b""),
                    Err(Error::UnusablePath(_))
                ),
                "{path}"
            );
        }
    }
}
