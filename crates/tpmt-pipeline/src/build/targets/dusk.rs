//! A Dusklight mod bundle: `mod.json`, an `overlay/` remapped to Dusklight's
//! own naming, and `res/` carried across.
//!
//! Not implemented. It rebuilds the overlay the same way `patch` does.
//!
//! What it will need that neither other target does: `mod/res/`, `mod.json`,
//! the generated `res/main.luau` glue, and the overlay path rewrite for
//! Dusklight's duplicated archive root names. Nothing of that belongs in the
//! shared build; this is where it lands.

use std::path::{Path, PathBuf};

use crate::build::{Job, Target};
use crate::{Error, Result};

/// # Errors
///
/// Always [`Error::Unsupported`], until there is something here.
pub const fn write(_job: &Job, _out: &Path) -> Result<PathBuf> {
    Err(Error::Unsupported(Target::Dusk))
}
