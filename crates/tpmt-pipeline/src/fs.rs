//! Getting bytes onto disk, with every error naming the path it hit.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::{Error, Result};

pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io_at(path))
}

/// Writes `data` to `path`, creating whatever directories it takes to get
/// there.
pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    fs::write(path, data).map_err(io_at(path))
}

pub fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

/// Like [`fs::remove_dir_all`], but a missing `path` is not an error: there
/// is already nothing there to clear.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
