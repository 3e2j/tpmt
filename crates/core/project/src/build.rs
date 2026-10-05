//! Where a build writes: its target's own directory under `build/targets/`,
//! or an empty one somebody named.
//!
//! The output is staged beside where it goes and swapped in only once the
//! whole build succeeded. A build that fails leaves the last good one where
//! it was.

use std::path::Path;

use crate::io::{Staging, refuse_unowned};
use crate::{Project, Result};

/// Build output, one directory per target under [`TARGETS_DIR`].
pub const DIR: &str = "build";
pub const TARGETS_DIR: &str = "targets";

impl Project {
    /// Stages the output for a build of `target`.
    ///
    /// The target's own directory under `build/targets/` is replaced whole on
    /// every build. An `output` given instead can be anywhere, so it must be
    /// missing or empty rather than emptied.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`](crate::Error::ForeignDirectory) if
    ///   `output` is not empty
    /// - [`Error::Io`](crate::Error::Io) if the staging directory cannot be
    ///   made
    pub fn stage_output(&self, target: &str, output: Option<&Path>) -> Result<Staging> {
        let out = match output {
            Some(output) => {
                refuse_unowned(output, &[])?;
                output.to_path_buf()
            }
            None => self.target_output(target),
        };
        Staging::begin(&out)
    }

    /// Where one build target writes what it produced. A target owns its
    /// directory outright and clears it on every build, so two targets never
    /// read each other's leftovers.
    #[must_use]
    pub fn target_output(&self, target: &str) -> std::path::PathBuf {
        self.root.join(DIR).join(TARGETS_DIR).join(target)
    }
}
