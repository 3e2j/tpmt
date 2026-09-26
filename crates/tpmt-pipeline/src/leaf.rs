//! One leaf file's bytes in and out of a project, with `mod/overlay/` laid
//! over `base/` the same way a build reads them.

use std::path::{Component, Path};

use crate::{Error, Result, fs, project};

pub fn read(project: &Path, path: &str) -> Result<Vec<u8>> {
    let at = checked(path)?;
    let file = [project::overlay(project), project::base(project)]
        .into_iter()
        .map(|layer| layer.join(at))
        .find(|file| file.is_file())
        .ok_or_else(|| Error::MissingFile(path.to_string()))?;
    fs::read(&file)
}

pub fn write(project: &Path, path: &str, data: &[u8]) -> Result<()> {
    fs::write(&project::overlay(project).join(checked(path)?), data)
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
    use crate::test_support::Scratch;

    const PATH: &str = "files/res/a.arc/m.bmg";

    fn project(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        fs::write(&project::base(&scratch.0).join(PATH), b"vanilla").unwrap();
        scratch
    }

    #[test]
    fn a_read_takes_the_overlay_over_base() {
        let scratch = project("leaf-read");
        assert_eq!(read(&scratch.0, PATH).unwrap(), b"vanilla");

        write(&scratch.0, PATH, b"edited").unwrap();
        assert_eq!(read(&scratch.0, PATH).unwrap(), b"edited");
        assert_eq!(
            fs::read(&project::base(&scratch.0).join(PATH)).unwrap(),
            b"vanilla"
        );
    }

    #[test]
    fn a_path_in_neither_layer_is_missing() {
        let scratch = project("leaf-missing");
        assert!(matches!(
            read(&scratch.0, "files/none.bmg"),
            Err(Error::MissingFile(path)) if path == "files/none.bmg"
        ));
        assert!(matches!(
            read(&scratch.0, "files/res"),
            Err(Error::MissingFile(_))
        ));
    }

    #[test]
    fn a_path_that_leaves_the_layer_is_refused() {
        let scratch = project("leaf-escape");
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "files/../../outside",
            "./files",
        ] {
            assert!(
                matches!(read(&scratch.0, path), Err(Error::UnusablePath(_))),
                "{path}"
            );
            assert!(
                matches!(write(&scratch.0, path, b""), Err(Error::UnusablePath(_))),
                "{path}"
            );
        }
    }
}
