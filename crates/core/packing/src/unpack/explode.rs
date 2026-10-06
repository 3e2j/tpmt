//! Taking one disc file apart into the project files it becomes.
//!
//! Disc files are often compressed (Yaz0, Yay0), archived (RARC), or both,
//! nested any depth. Exploding peels all of it so the project holds each
//! payload as a plain file, and `implode` packs it back for a build.
//!
//! [`explode`] hands each project file to a sink the moment it's peeled, so
//! the unpack writes it to disk straight away instead of holding the whole
//! disc file's contents in memory. A compressed archive holding one
//! compressed message file peels like this:
//!
//! ```text
//! zel_00.arc                 Yaz0   wrapper     peeled
//! zel_00.arc                 RARC   archive     opened
//! zel_00.arc/a.bmg           Yaz0   wrapper     peeled
//! zel_00.arc/a.bmg           MESG   payload     to the sink
//! zel_00.arc/.tpmt-arc.toml         sidecar     to the sink
//! ```
//!
//! The sidecar records each member's path, id, preload flag and wrapper, so
//! a build can pack the archive back.
//!
//! Only the magic decides what a file is, since some retail names lie about
//! their contents.
//!
//! A wrapper is recorded by whatever holds the file: the sidecar for a
//! member, the unpack's caller for a disc file.

use std::borrow::Cow;

use tpmt_archive::Archive;
use tpmt_archive::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_binary::{Compression, FileKind, Format};

use super::File;
use crate::{Error, Result};

/// What went wrong decoding one file, without where.
///
/// [`Error::Decode`] adds the path, attached here at the innermost point so a
/// member of a nested archive names itself rather than the disc file it came
/// in.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Archive(#[from] tpmt_archive::Error),

    #[error(transparent)]
    Compress(#[from] tpmt_compression::Error),
}

/// Peels `data`'s compression wrapper, if any, and sinks what's left.
///
/// An archive is exploded member by member, then its sidecar is sunk.
/// Returns the wrapper that came off, for whatever holds `data` to record.
///
/// # Errors
///
/// - [`Error::Decode`] naming the innermost file whose compression wrapper
///   or archive would not open
/// - whatever `sink` returns
pub fn explode<E: From<Error>>(
    path: &str,
    data: &[u8],
    sink: &mut impl FnMut(File<'_>) -> Result<(), E>,
) -> Result<Option<Compression>, E> {
    // Peel off compression
    let compression = FileKind::identify(data).and_then(FileKind::compression);
    let bare = match compression {
        Some(compression) => {
            Cow::Owned(tpmt_compression::decompress(compression, data).map_err(at(path))?)
        }
        None => Cow::Borrowed(data),
    };

    // Sink non-archives
    let kind = FileKind::identify(&bare);
    if kind != Some(FileKind::Rarc) {
        sink(File {
            path,
            kind,
            bytes: &bare,
        })?;
        return Ok(compression);
    }

    // Decode archives (and explode members)
    let archive = Archive::decode(&bare).map_err(at(path))?;
    let mut members = Vec::with_capacity(archive.files.len());
    for member in &archive.files {
        // The sidecar records the compression the member's own bytes have,
        // not what the archive's entry claims, so it keeps what the file
        // really is.
        let compression = explode(&format!("{path}/{}", member.path), member.data, sink)?;
        // TODO: warn when the entry's claim (`member.compression`) disagrees.
        // Doesn't affect the output, just a warn.
        members.push(Member {
            path: member.path.clone(),
            preload: member.preload,
            compression,
            id: member.id,
        });
    }

    // Write sidecar
    let toml = Sidecar::new(archive.root, members)
        .to_toml()
        .map_err(at(path))?;
    sink(File {
        path: &format!("{path}/{SIDECAR}"),
        kind: None,
        bytes: toml.as_bytes(),
    })?;
    Ok(compression)
}

fn at<E: Into<DecodeError>>(path: &str) -> impl FnOnce(E) -> Error + '_ {
    move |source| Error::Decode {
        path: path.to_string(),
        source: source.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tpmt_archive::File;
    use tpmt_compression::Strategy;

    use super::*;

    fn wrap(compression: Compression, data: &[u8]) -> Vec<u8> {
        tpmt_compression::compress(compression, data, Strategy::Parity).unwrap()
    }

    fn archive(root: &str, files: Vec<File<'_>>) -> Vec<u8> {
        Archive {
            root: root.to_string(),
            files,
            next_free_id: None,
        }
        .encode()
        .unwrap()
    }

    fn file<'a>(path: &str, data: &'a [u8]) -> File<'a> {
        File {
            path: path.to_string(),
            data,
            ..Default::default()
        }
    }

    /// What one call to [`super::explode`] made of a file.
    #[derive(Debug)]
    struct Exploded {
        /// Everything it became, keyed by project path.
        outputs: BTreeMap<String, Vec<u8>>,
        compression: Option<Compression>,
    }

    fn explode(path: &str, data: &[u8]) -> Result<Exploded> {
        let mut outputs = BTreeMap::new();
        let compression = super::explode(path, data, &mut |file| -> Result<()> {
            outputs.insert(file.path.to_string(), file.bytes.to_vec());
            Ok(())
        })?;
        Ok(Exploded {
            outputs,
            compression,
        })
    }

    fn sidecar(outputs: &BTreeMap<String, Vec<u8>>, dir: &str) -> Sidecar {
        let bytes = &outputs[&format!("{dir}/{SIDECAR}")];
        Sidecar::from_toml(std::str::from_utf8(bytes).unwrap()).unwrap()
    }

    /// A blob nothing recognises comes through bare, and the wrapper that
    /// came off it is reported for the caller to record.
    #[test]
    fn unrecognised_bytes_pass_through_unwrapped() {
        let Exploded {
            outputs,
            compression,
        } = explode("files/thing.bin", &wrap(Compression::Yaz0, b"not a format")).unwrap();
        assert_eq!(compression, Some(Compression::Yaz0));
        assert_eq!(
            outputs,
            BTreeMap::from([("files/thing.bin".to_string(), b"not a format".to_vec())])
        );
    }

    /// A wrapped archive holding a wrapped member and a wrapped nested
    /// archive. Every wrapper comes off, whichever it is, and whatever held
    /// the file records it exactly once.
    #[test]
    fn wrapping_is_recorded_by_the_container() {
        let inner = wrap(
            Compression::Yaz0,
            &archive("inner", vec![file("deep.bin", b"deep")]),
        );
        let member = wrap(Compression::Yay0, b"member");
        let outer = wrap(
            Compression::Yaz0,
            &archive(
                "outer",
                vec![
                    file("inner.arc", &inner),
                    file("wrapped.bin", &member),
                    file("plain.bin", b"plain"),
                ],
            ),
        );

        let Exploded {
            outputs,
            compression,
        } = explode("files/outer.arc", &outer).unwrap();
        assert_eq!(
            compression,
            Some(Compression::Yaz0),
            "the disc file's wrapper goes up to the caller, not to disk"
        );

        assert_eq!(
            outputs.keys().collect::<Vec<_>>(),
            [
                "files/outer.arc/.tpmt-arc.toml",
                "files/outer.arc/inner.arc/.tpmt-arc.toml",
                "files/outer.arc/inner.arc/deep.bin",
                "files/outer.arc/plain.bin",
                "files/outer.arc/wrapped.bin",
            ]
        );
        assert_eq!(outputs["files/outer.arc/wrapped.bin"], b"member");
        assert_eq!(outputs["files/outer.arc/inner.arc/deep.bin"], b"deep");

        let outer = sidecar(&outputs, "files/outer.arc");
        let wrapped: Vec<_> = outer
            .members
            .iter()
            .map(|member| (member.path.as_str(), member.compression))
            .collect();
        assert_eq!(
            wrapped,
            [
                ("inner.arc", Some(Compression::Yaz0)),
                ("wrapped.bin", Some(Compression::Yay0)),
                ("plain.bin", None)
            ]
        );
    }

    /// The innermost path, not the disc file's, names a failure.
    #[test]
    fn errors_name_the_member_that_failed() {
        let mut truncated = wrap(Compression::Yaz0, b"member bytes that will be cut short");
        truncated.truncate(12);
        let outer = archive("outer", vec![file("bad.bin", &truncated)]);

        let err = explode("files/outer.arc", &outer).unwrap_err();
        assert!(
            matches!(&err, Error::Decode { path, .. } if path == "files/outer.arc/bad.bin"),
            "{err}"
        );
    }
}
