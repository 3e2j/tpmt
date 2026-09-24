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

// TODO: an option to export through Dusklight's services instead of overlay
// files: `MessageService` for messages, `FlowService` for flows, and others
// where they fit. Flows could then name a mod's own queries and events next
// to the built-in ones, with the export doing the wiring and the modder
// writing only what each one does. Worth revisiting once those services
// mature.

/// # Errors
///
/// Always [`Error::Unsupported`], until there is something here.
pub const fn write(_job: &Job, _out: &Path) -> Result<PathBuf> {
    Err(Error::Unsupported(Target::Dusk))
}
