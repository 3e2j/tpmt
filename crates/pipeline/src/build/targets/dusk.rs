//! A Dusklight mod bundle: `mod.json`, an `overlay/` built from `mod/changes/`
//! and remapped to Dusklight's own naming, and `textures/` and `res/` carried
//! across.
//!
//! Not implemented. It rebuilds the changed files the same way `patch` does.
//!
//! What it will need that neither other target does: the mod's own resources and metadata,
//! which [`Job`](crate::Job) doesn't carry yet, the generated `res/main.luau` glue,
//! and the overlay path rewrite for Dusklight's duplicated archive root names.
//! Nothing of that belongs in the shared build; this is where it lands.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::build::{Context, Target};

// TODO: an option to export through Dusklight's services instead of overlay
// files: `MessageService` for messages, `FlowService` for flows, and others
// where they fit. Flows could then name a mod's own queries and events next
// to the built-in ones, with the export doing the wiring and the modder
// writing only what each one does. Worth revisiting once those services
// mature.

// TODO: warn on game code in `changes/`: `.dol`, `.rel`, `.str` and `.map`.
// Dusklight compiles the game in and never loads these, so replaced code
// won't run. Point the modder at hooks instead.

/// # Errors
///
/// [`Error::Unsupported`] until there is something here.
pub fn write<E: From<Error>>(_context: &Context<'_, E>, _out: &Path) -> Result<PathBuf, E> {
    Err(Error::Unsupported(Target::Dusk).into())
}
