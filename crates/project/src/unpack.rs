//! Making a project from a disc: `tpmt-pipeline` walks it, and every file it
//! hands over lands in `base/`.

use std::path::Path;

use tpmt_report::{Progress, Step};

use crate::io::{Staging, fs};
use crate::store::{Digests, Formats, digest};
use crate::{Project, Result, base};

impl Project {
    /// Unpacks the disc at `iso` into a project at `root`, and returns it.
    ///
    /// Writes `base/` and `.tpmt/`, and scaffolds an empty `mod/` next to
    /// them. `root` may be an existing project. In that case the unpack
    /// replaces only `base/` and leaves `mod/` alone. It commits only once
    /// every file is written, so a failure part way through leaves no
    /// half-made project.
    ///
    /// Reports [`Step::Unpack`] across the whole image, then [`Step::Save`],
    /// through `progress`. Each file's reports go to `progress` as soon as
    /// that file is unpacked.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`](crate::Error::ForeignDirectory) if
    ///   `root` holds something else
    /// - [`Error::Pipeline`](crate::Error::Pipeline) if the ISO can't be read,
    ///   or a file on it isn't what its bytes claim
    /// - [`Error::Io`](crate::Error::Io) on any write
    pub fn unpack(iso: &Path, root: &Path, progress: &Progress) -> Result<Self> {
        let project = Self::claim(root)?;
        let staging = Staging::begin(&project.base())?;
        let base = staging.dir();

        let unpacked = tpmt_pipeline::unpack(iso, progress, |leaf| -> Result<_> {
            fs::write(&base.join(leaf.path), leaf.bytes)?;
            Ok((leaf.path.to_string(), digest(leaf.bytes), leaf.kind))
        })?;
        // Writing a file already made its parents. This catches the empty ones.
        for dir in &unpacked.directories {
            fs::create_dir_all(&base.join(dir))?;
        }

        progress.begin(Step::Save, 0);
        base::write(base, &unpacked.metadata, unpacked.yaz0_compressed)?;
        staging.promote()?;

        let mut digests = Digests::new();
        let mut formats = Formats::new();
        for (path, digest, kind) in unpacked.stored {
            if let Some(kind) = kind {
                formats.entry(kind).or_default().insert(path.clone());
            }
            digests.insert(path, digest);
        }
        project.write_store(iso, &unpacked.metadata.boot, &digests, &formats)?;
        project.scaffold_mod()?;

        Self::discover(root)
    }
}
