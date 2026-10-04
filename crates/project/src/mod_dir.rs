//! `mod/`: the only directory a modder edits. tpmt writes its skeleton once
//! and never touches it again.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Result;
use crate::io::Staging;
use crate::io::fs::{create_dir_all, write_json};

/// The mod project: `overlay/`, `res/`, `mod.json`.
pub const DIR: &str = "mod";
const OVERLAY_DIR: &str = "overlay";
const RES_DIR: &str = "res";
const SCRIPTS_DIR: &str = "scripts";
const MOD_JSON: &str = "mod.json";

/// `overlay/` under the `mod/` at `mod_dir`.
pub fn overlay(mod_dir: &Path) -> PathBuf {
    mod_dir.join(OVERLAY_DIR)
}

/// `mod.json`: what a mod says about itself.
///
/// Fields are the target-agnostic subset only. Dusklight reads a few more
/// (`runtime`, pinning a mod to a specific host runtime service) that are
/// specific to the `.dusk` export step.
#[derive(Serialize)]
struct ModMetadata<'a> {
    id: &'a str,
    name: &'a str,
    version: &'a str,
    author: &'a str,
    description: &'a str,
    icon: Option<&'a str>,
    banner: Option<&'a str>,
}

/// Writes the skeleton at `mod_dir` with a starter `mod.json` naming `id`,
/// unless something is already there. See
/// [`Project::scaffold_mod`](crate::Project::scaffold_mod).
pub fn scaffold(mod_dir: &Path, id: &str) -> Result<()> {
    if mod_dir.is_dir() {
        return Ok(());
    }

    // Staged, so a failure part way cannot leave a `mod/` without its
    // `mod.json` that the check above would then skip forever.
    let staging = Staging::begin(mod_dir)?;
    create_dir_all(&overlay(staging.dir()))?;
    create_dir_all(&staging.dir().join(RES_DIR).join(SCRIPTS_DIR))?;
    write_json(
        &staging.dir().join(MOD_JSON),
        &ModMetadata {
            id,
            name: id,
            version: "0.1.0",
            author: "",
            description: "",
            icon: None,
            banner: None,
        },
    )?;
    staging.promote()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::io::fs::read;

    #[test]
    fn scaffold_never_clobbers_an_existing_mod() {
        let scratch = tempfile::tempdir().unwrap();
        let mod_dir = scratch.path().join(DIR);
        scaffold(&mod_dir, "test").unwrap();
        let json = mod_dir.join(MOD_JSON);
        fs::write(&json, b"edited").unwrap();

        scaffold(&mod_dir, "test").unwrap();
        assert_eq!(read(&json).unwrap(), b"edited");
    }
}
