//! The few file operations a build does on its own output, with every error
//! naming the path it hit.

use std::fs::{self, File};
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

use crate::{Error, Result};

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Writes `data` to `path`, creating any missing parent directories.
pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io(parent))?;
    }
    fs::write(path, data).map_err(io(path))
}

/// Reads a whole file. The workspace disallows `std::fs::read`, since an ISO
/// won't fit in memory, and nothing read here is one.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(io(path))?;
    let mut data = Vec::new();
    BufReader::new(file)
        .read_to_end(&mut data)
        .map_err(io(path))?;
    Ok(data)
}

pub fn create(path: &Path) -> Result<File> {
    File::create(path).map_err(io(path))
}

/// How long a file is, without reading it.
pub fn len(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(io(path))?.len())
}

/// Like [`fs::remove_dir_all`], but a missing `path` is not an error.
pub fn remove_dir_all_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(source) if source.kind() != ErrorKind::NotFound => Err(io(path)(source)),
        _ => Ok(()),
    }
}
