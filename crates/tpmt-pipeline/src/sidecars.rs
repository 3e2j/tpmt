//! Sidecars hold what a decoded format can't represent as editable files, so
//! the build can restore it without needing the original disc.
//!
//! A sidecar is its format crate's own, encoding included, the same as an
//! editable form is. What's here per format is only what a format crate has
//! no way to know: where it sits in a project, and what a missing one means.

pub(crate) mod arc {
    //! Where an archive's sidecar sits in a project, and what a missing one
    //! means. Reading and writing it is [`tpmt_arc::editable::sidecar::Sidecar`]'s own.

    use std::path::Path;

    use tpmt_arc::editable::sidecar::{SIDECAR, Sidecar};

    use crate::Error;
    use crate::fs::{read, write};

    /// Reads the sidecar at the root of an unpacked archive.
    ///
    /// A missing one is an error here rather than a
    /// [`fresh`](tpmt_arc::editable::sidecar::Sidecar::fresh) archive, because on a
    /// directory that came off the disc it means the sidecar was lost:
    /// defaulting there would quietly throw away the memory every member was
    /// loaded into and the wrapper the archive arrived under. Only a caller
    /// that knows the disc never had this archive can say the missing file is
    /// nothing to worry about.
    pub(crate) fn read_sidecar(directory: &Path) -> Result<Sidecar, Error> {
        let path = directory.join(SIDECAR);
        if !path.exists() {
            return Err(Error::LostSidecar(path));
        }

        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        Sidecar::from_toml(&text).map_err(|source| Error::Archive { path, source })
    }

    /// Writes the sidecar, giving back the bytes it wrote, since change
    /// detection hashes what went into the project rather than reading it
    /// back.
    pub(crate) fn write_sidecar(sidecar: &Sidecar, directory: &Path) -> Result<Vec<u8>, Error> {
        let text = sidecar.to_toml();
        write(&directory.join(SIDECAR), text.as_bytes())?;
        Ok(text.into_bytes())
    }
}
