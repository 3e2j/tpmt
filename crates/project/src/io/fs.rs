//! Getting bytes onto disk, with every error naming the path it hit.

use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

/// Like [`fs::create_dir_all`], naming `path` on failure.
///
/// # Errors
///
/// - [`Error::Io`] if a directory cannot be made
pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(Error::io(path))
}

/// Writes `data` to `path`, creating any missing parent directories.
///
/// # Errors
///
/// - [`Error::Io`] if a parent directory or the file cannot be written
pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    fs::write(path, data).map_err(Error::io(path))
}

/// Writes `value` to `path` as TOML.
///
/// # Errors
///
/// - [`Error::Serialize`] if `value` will not serialize
/// - [`Error::Io`] on the write
pub fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

/// Writes `value` to `path` as JSON.
///
/// # Errors
///
/// - [`Error::Serialize`] if `value` will not serialize
/// - [`Error::Io`] on the write
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|source| Error::Serialize {
        path: path.to_path_buf(),
        source: source.into(),
    })?;
    write(path, text.as_bytes())
}

/// Like [`fs::remove_dir_all`], but a missing `path` is not an error, since
/// there is nothing to clear.
///
/// # Errors
///
/// - [`Error::Io`] if `path` exists and cannot be removed
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

/// Reads a whole file. The workspace disallows `std::fs::read` because an ISO
/// will not fit in memory. Everything this is used for is a project file,
/// where the largest thing on the disc is a 137 MB video.
///
/// # Errors
///
/// - [`Error::Io`] if the file cannot be opened or read
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(Error::io(path))?;
    let mut data = Vec::new();
    BufReader::new(file)
        .read_to_end(&mut data)
        .map_err(Error::io(path))?;
    Ok(data)
}

/// Like [`read`], but `None` when there is no file at `path`, either because
/// nothing is there or because a directory is.
///
/// # Errors
///
/// - [`Error::Io`] on any other failure to read
pub fn read_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match read(path) {
        Ok(data) => Ok(Some(data)),
        // Asked only after the read failed. A directory fails differently per
        // platform (IsADirectory on Linux, PermissionDenied on Windows).
        Err(Error::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound || path.is_dir() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Like [`fs::rename`], but a missing `from` is not an error, since there is
/// nothing to move.
///
/// # Errors
///
/// - [`Error::Io`] if `from` exists and cannot be moved
pub fn rename_if_exists(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: from.to_path_buf(),
            source,
        }),
    }
}

/// How long a file is, without reading it.
///
/// # Errors
///
/// - [`Error::Io`] if `path`'s metadata cannot be read
pub fn len(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(Error::io(path))?.len())
}

/// Reads `path` back as TOML.
///
/// # Errors
///
/// - [`Error::Io`] if it cannot be read
/// - [`Error::Parse`] if it is not UTF-8 or not a `T`
pub fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(Error::parse(path))?;
    toml::from_str(text).map_err(Error::parse(path))
}
