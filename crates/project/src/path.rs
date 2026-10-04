//! Project paths: UTF-8, `/`-separated, relative to a layer root, the same
//! in `base/` and `mod/overlay/`. `files/res/Msgus/bmgres3.arc/zel_00.bmg` is
//! one.

use std::fs;
use std::path::{Component, Path};

use crate::{Error, Result};

/// `path` under the project path `under`, which may be the root.
#[must_use]
pub fn join(under: &str, path: &str) -> String {
    if under.is_empty() {
        path.to_string()
    } else {
        format!("{under}/{path}")
    }
}

/// Every file under `dir`, as sorted project paths.
///
/// # Errors
///
/// - [`Error::Io`] if a directory cannot be listed
/// - [`Error::UnusablePath`] if a name in it is not UTF-8
pub fn files(dir: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((dir, at)) = pending.pop() {
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir).map_err(Error::io(&dir))? {
            let entry = entry.map_err(Error::io(&dir))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| Error::UnusablePath(entry.path()))?;
            let path = join(&at, name);
            if entry.path().is_dir() {
                pending.push((entry.path(), path));
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// `path` as a relative path that stays inside the layer it's joined to.
pub fn checked(path: &str) -> Result<&Path> {
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
