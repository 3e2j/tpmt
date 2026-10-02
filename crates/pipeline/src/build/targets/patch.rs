//! Every disc file the overlay changed, at the path the disc holds it under,
//! so the output drops over an extracted game.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::build::{Job, rebuild};

/// Writes the changed disc files into `out`, which is itself the result, so
/// the path returned is empty.
///
/// # Errors
///
/// Whatever assembling one of them hit. See [`crate::build::run`].
pub fn write(job: &Job, out: &Path) -> Result<PathBuf> {
    rebuild(job, out)?;
    Ok(PathBuf::new())
}
