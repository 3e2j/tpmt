//! Every disc file the edits changed, at the path the disc holds it under,
//! so the output drops over an extracted game.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::build::{Context, rebuild};

/// Writes the changed disc files into `out`, which is itself the result, so
/// the path returned is empty.
///
/// # Errors
///
/// Whatever assembling one of them hit. See [`crate::build`].
pub fn write<E>(context: &Context<'_, E>, out: &Path) -> Result<PathBuf, E>
where
    E: From<Error> + Send,
{
    rebuild(context, out)?;
    Ok(PathBuf::new())
}
