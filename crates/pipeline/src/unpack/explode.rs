//! Taking one disc file apart into the project files it becomes: a plain
//! file is itself, an archive is a directory of members plus a sidecar,
//! however deep the nesting goes.
//!
//! Detection is content-only. Some files on the retail disc carry a path or
//! extension that doesn't match what's inside, so nothing here branches on a
//! name, only on each format's magic. A blob nothing recognises passes
//! through unchanged, whatever its name claims.
//!
//! [`file`] peels a compression wrapper before sniffing content, since most
//! files on disc arrive wrapped and nothing downstream expects to see it.
//! It hands back which wrapper came off, since the record of it belongs to
//! whatever holds the file. An archive writes it on the member's sidecar
//! entry, and the unpack's caller keeps it for a disc file. A file never
//! records its own.
//!
//! An archive entry also states its member's compression in its flags. The
//! magic still decides what gets peeled, and a flag that disagrees with it
//! becomes a warning [`Report`]. A build writes the flags from the bytes, so
//! the rebuilt entry won't match the original.
//!
//! The sink sees every [`Layer`] on the way down, not only what lands in the
//! project, so a caller can read a wrapper or an archive as the disc holds it.

use std::borrow::Cow;

use tpmt_archive::Archive;
use tpmt_archive::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_binary::{Compression, FileKind, Format};
use tpmt_report::Report;

use crate::{Error, Result};

fn describe(compression: Option<Compression>) -> &'static str {
    compression.map_or("uncompressed", Compression::name)
}

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

/// One step on the way from a disc file to the project files it becomes.
///
/// A compression wrapper and the bytes it holds share a path, told apart by
/// `kind`.
#[derive(Debug, Clone, Copy)]
pub struct Layer<'a> {
    /// Its project path.
    pub path: &'a str,
    /// What its magic says it is, if anything.
    pub kind: Option<FileKind>,
    pub bytes: &'a [u8],
    /// Whether the project stores it. A wrapper and an archive aren't
    /// stored, only what they hold and an archive's sidecar.
    pub leaf: bool,
}

/// Peels `data`, then hands it to whichever format's magic it opens with.
///
/// Calls `sink` with every [`Layer`], outermost first: the wrapper if there
/// is one, then a plain file, or an archive followed by each member's layers
/// and its sidecar. Returns the compression wrapper that came off `data`
/// first, if any, for the caller to record. Pushes a warning to `reports` for every member whose
/// entry misstates its compression.
///
/// Each format's magic picks it before its decoder runs, so an error out of
/// a decoder always means "this format, but broken", never "not this
/// format".
///
/// # Errors
///
/// - [`Error::Decode`] naming the innermost file whose compression wrapper
///   or archive would not open
/// - whatever `sink` returns
pub fn file<E: From<Error>>(
    path: &str,
    data: &[u8],
    sink: &mut impl FnMut(Layer<'_>) -> Result<(), E>,
    reports: &mut Vec<Report>,
) -> Result<Option<Compression>, E> {
    // On this disc the wrapper is a convention of where a file sits, not
    // something the file itself declares, so the caller records it.
    let kind = FileKind::identify(data);
    let compression = kind.and_then(FileKind::compression);
    let bare = match compression {
        Some(compression) => {
            sink(Layer {
                path,
                kind,
                bytes: data,
                leaf: false,
            })?;
            Cow::Owned(tpmt_compression::decompress(compression, data).map_err(at(path))?)
        }
        None => Cow::Borrowed(data),
    };

    let inner = FileKind::identify(&bare);
    let is_archive = inner == Some(FileKind::Rarc);
    // Leaf formats pass through as raw bytes. Decoding one is a separate,
    // on-demand call.
    sink(Layer {
        path,
        kind: inner,
        bytes: &bare,
        leaf: !is_archive,
    })?;
    if is_archive {
        let decoded = Archive::decode(&bare).map_err(at(path))?;
        archive(path, decoded, sink, reports)?;
    }

    Ok(compression)
}

/// Sinks every member of `archive`, then a [`SIDECAR`] recording each
/// member's path, preload flag, id, and compression wrapper.
fn archive<E: From<Error>>(
    path: &str,
    archive: Archive<'_>,
    sink: &mut impl FnMut(Layer<'_>) -> Result<(), E>,
    reports: &mut Vec<Report>,
) -> Result<(), E> {
    let mut members = Vec::with_capacity(archive.files.len());

    for member in &archive.files {
        let member_path = format!("{path}/{}", member.path);
        let found = Compression::of(member.data);
        if member.compression != found {
            reports.push(Report::warn(format_args!(
                "`{member_path}`: its archive entry says {} but its bytes are {}, so a build will rewrite the entry",
                describe(member.compression),
                describe(found)
            )));
        }
        let compression = file(&member_path, member.data, sink, reports)?;
        members.push(Member {
            path: member.path.clone(),
            preload: member.preload,
            compression,
            id: member.id,
        });
    }

    let toml = Sidecar::new(archive.root, members)
        .to_toml()
        .map_err(at(path))?;
    sink(Layer {
        path: &format!("{path}/{SIDECAR}"),
        kind: None,
        bytes: toml.as_bytes(),
        leaf: true,
    })
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

    /// What one call to [`super::file`] made of a file.
    #[derive(Debug)]
    struct Exploded {
        /// Everything it became, keyed by project path.
        outputs: BTreeMap<String, Vec<u8>>,
        compression: Option<Compression>,
        reports: Vec<Report>,
    }

    fn explode(path: &str, data: &[u8]) -> Result<Exploded> {
        let mut outputs = BTreeMap::new();
        let mut reports = Vec::new();
        let compression = super::file(
            path,
            data,
            &mut |layer| -> Result<()> {
                if layer.leaf {
                    outputs.insert(layer.path.to_string(), layer.bytes.to_vec());
                }
                Ok(())
            },
            &mut reports,
        )?;
        Ok(Exploded {
            outputs,
            compression,
            reports,
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
            ..
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
            reports,
        } = explode("files/outer.arc", &outer).unwrap();
        assert_eq!(
            compression,
            Some(Compression::Yaz0),
            "the disc file's wrapper goes up to the caller, not to disk"
        );
        assert_eq!(reports, [], "a packed archive states what its bytes are");

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

    /// The sink sees every wrapper and archive on the way down, outermost
    /// first, with only what the project stores marked as a leaf.
    #[test]
    fn every_layer_reaches_the_sink() {
        let member = wrap(Compression::Yaz0, b"member");
        let outer = wrap(
            Compression::Yaz0,
            &archive("outer", vec![file("wrapped.bin", &member)]),
        );

        let mut layers = Vec::new();
        super::file(
            "files/outer.arc",
            &outer,
            &mut |layer| -> Result<()> {
                layers.push((layer.path.to_string(), layer.kind, layer.leaf));
                Ok(())
            },
            &mut Vec::new(),
        )
        .unwrap();

        let layer = |path: &str, kind, leaf| (path.to_string(), kind, leaf);
        assert_eq!(
            layers,
            [
                layer("files/outer.arc", Some(FileKind::Yaz0), false),
                layer("files/outer.arc", Some(FileKind::Rarc), false),
                layer("files/outer.arc/wrapped.bin", Some(FileKind::Yaz0), false),
                layer("files/outer.arc/wrapped.bin", None, true),
                layer("files/outer.arc/.tpmt-arc.toml", None, true),
            ]
        );
    }

    /// An entry that calls a wrapped member uncompressed is warned about, and
    /// the magic still decides that the wrapper comes off.
    #[test]
    fn misstated_compression_is_warned_about() {
        let member = wrap(Compression::Yaz0, b"member");
        let mut outer = archive("outer", vec![file("wrapped.bin", &member)]);

        // The entry list's offset sits 0xC into the data header at 0x20, and
        // counts from it. The lone file is the first entry, its flags 4 in.
        let at = 0x20 + 0xC;
        let entries = u32::from_be_bytes(outer[at..at + 4].try_into().unwrap()) as usize;
        outer[0x20 + entries + 4] &= !(0x04 | 0x80);

        let Exploded {
            outputs, reports, ..
        } = explode("files/outer.arc", &outer).unwrap();
        assert_eq!(outputs["files/outer.arc/wrapped.bin"], b"member");
        assert_eq!(
            reports,
            [Report::warn(
                "`files/outer.arc/wrapped.bin`: its archive entry says uncompressed but its bytes are Yaz0, so a build will rewrite the entry"
            )]
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
