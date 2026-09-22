//! Getting bytes onto disk, with every error naming the path it hit.

use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io_at(path))
}

/// Writes `data` to `path`, creating any missing parent directories.
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

/// Like [`fs::remove_dir_all`], but a missing `path` is not an error, since
/// there is nothing to clear.
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

/// Reads a whole file. The workspace disallows `std::fs::read` because an ISO
/// will not fit in memory. Everything this is used for is a project file,
/// where the largest thing on the disc is a 137 MB video.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let file = fs::File::open(path).map_err(io_at(path))?;
    let mut data = Vec::new();
    BufReader::new(file)
        .read_to_end(&mut data)
        .map_err(io_at(path))?;
    Ok(data)
}

/// How long a file is, without reading it.
pub fn len(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(io_at(path))?.len())
}

pub fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(parse_at(path))?;
    toml::from_str(text).map_err(parse_at(path))
}

/// [`io_at`] for a file that read fine and then would not parse.
pub fn parse_at<E>(path: &Path) -> impl FnOnce(E) -> Error + '_
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    move |source| Error::Parse {
        path: path.to_path_buf(),
        source: source.into(),
    }
}
