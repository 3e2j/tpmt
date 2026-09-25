//! Taking one disc file apart into the project files it becomes: a plain
//! file is itself, an archive is a directory of members plus a sidecar,
//! however deep the nesting goes.
//!
//! Detection is content-only. Some files on the retail disc carry a path or
//! extension that doesn't match what's inside, so nothing here branches on a
//! name, only on each format's magic. A blob nothing recognises passes
//! through unchanged, whatever its name claims.
//!
//! [`file`] peels Yaz0 before sniffing content, since most files on disc
//! arrive wrapped and nothing downstream expects to see it. It hands back
//! whether a wrapper came off, since the record of it belongs to whatever
//! holds the file. An archive writes it on the member's sidecar entry, the
//! disc on `yaz0.toml`. A file never records its own.

use tpmt_format::Format;
use tpmt_jkernel_arc::Archive;
use tpmt_jkernel_arc::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_jkernel_compress::{is_yaz0, yaz0_decode};

use crate::{Error, Result};

/// What went wrong decoding one file, without where. [`Error::Decode`] adds
/// the path, attached here at the innermost point so a member of a nested
/// archive names itself rather than the disc file it came in.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Archive(#[from] tpmt_jkernel_arc::Error),

    #[error(transparent)]
    Compress(#[from] tpmt_jkernel_compress::Error),
}

/// Peels `data`, then hands it to whichever format's magic it opens with.
///
/// Calls `sink` with every `(project path, bytes)` that comes out: once
/// for a plain file, once per member plus once for the sidecar of an
/// archive. Returns whether a Yaz0 wrapper came off `data` first, for the
/// caller to record.
///
/// Each format's magic picks it before its decoder runs, so an error out of
/// a decoder always means "this format, but broken", never "not this
/// format".
///
/// # Errors
///
/// - [`Error::Decode`] naming the innermost file whose Yaz0 wrapper or
///   archive would not open
/// - whatever `sink` returns
pub fn file(
    path: &str,
    data: &[u8],
    sink: &mut impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<bool> {
    let yaz0_compressed = is_yaz0(data);
    let unwrapped = yaz0_compressed
        .then(|| yaz0_decode(data))
        .transpose()
        .map_err(at(path))?;
    let bare = unwrapped.as_deref().unwrap_or(data);

    if Archive::recognises(bare) {
        archive(path, bare, sink)?;
    } else {
        // Leaf formats pass through as raw bytes. Editing is the UI's job.
        sink(path, bare)?;
    }

    Ok(yaz0_compressed)
}

/// Sinks every member of the archive in `bare`, then a [`SIDECAR`] recording
/// each member's path, preload flag, id, and Yaz0 wrapper.
fn archive(
    path: &str,
    bare: &[u8],
    sink: &mut impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<()> {
    let archive = Archive::decode(bare).map_err(at(path))?;
    let mut members = Vec::with_capacity(archive.files.len());

    for member in &archive.files {
        let yaz0_compressed = file(&format!("{path}/{}", member.path), member.data, sink)?;
        members.push(Member {
            path: member.path.clone(),
            preload: member.preload,
            yaz0_compressed,
            id: member.id,
        });
    }

    let toml = Sidecar::new(archive.root, members)
        .to_toml()
        .map_err(at(path))?;
    sink(&format!("{path}/{SIDECAR}"), toml.as_bytes())
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

    use tpmt_jkernel_arc::File;
    use tpmt_jkernel_compress::yaz0_encode;

    use super::*;

    fn wrap(data: &[u8]) -> Vec<u8> {
        yaz0_encode(data, false).unwrap()
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

    /// Everything `data` explodes into, keyed by project path, plus whether
    /// it arrived wrapped.
    fn explode(path: &str, data: &[u8]) -> Result<(BTreeMap<String, Vec<u8>>, bool)> {
        let mut outputs = BTreeMap::new();
        let yaz0_compressed = super::file(path, data, &mut |path, data| {
            outputs.insert(path.to_string(), data.to_vec());
            Ok(())
        })?;
        Ok((outputs, yaz0_compressed))
    }

    fn sidecar(outputs: &BTreeMap<String, Vec<u8>>, dir: &str) -> Sidecar {
        let bytes = &outputs[&format!("{dir}/{SIDECAR}")];
        Sidecar::from_toml(std::str::from_utf8(bytes).unwrap()).unwrap()
    }

    /// A blob nothing recognises comes through bare, and the wrapper that
    /// came off it is reported for the caller to record.
    #[test]
    fn unrecognised_bytes_pass_through_unwrapped() {
        let (outputs, yaz0_compressed) =
            explode("files/thing.bin", &wrap(b"not a format")).unwrap();
        assert!(yaz0_compressed);
        assert_eq!(
            outputs,
            BTreeMap::from([("files/thing.bin".to_string(), b"not a format".to_vec())])
        );
    }

    /// A wrapped archive holding a wrapped member and a wrapped nested
    /// archive. Every wrapper comes off, and whatever held the file records
    /// it exactly once.
    #[test]
    fn wrapping_is_recorded_by_the_container() {
        let inner = wrap(&archive("inner", vec![file("deep.bin", b"deep")]));
        let member = wrap(b"member");
        let outer = wrap(&archive(
            "outer",
            vec![
                file("inner.arc", &inner),
                file("wrapped.bin", &member),
                file("plain.bin", b"plain"),
            ],
        ));

        let (outputs, yaz0_compressed) = explode("files/outer.arc", &outer).unwrap();
        assert!(
            yaz0_compressed,
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
            .map(|member| (member.path.as_str(), member.yaz0_compressed))
            .collect();
        assert_eq!(
            wrapped,
            [
                ("inner.arc", true),
                ("wrapped.bin", true),
                ("plain.bin", false)
            ]
        );
    }

    /// The innermost path, not the disc file's, names a failure.
    #[test]
    fn errors_name_the_member_that_failed() {
        let mut truncated = wrap(b"member bytes that will be cut short");
        truncated.truncate(12);
        let outer = archive("outer", vec![file("bad.bin", &truncated)]);

        let err = explode("files/outer.arc", &outer).unwrap_err();
        assert!(
            matches!(&err, Error::Decode { path, .. } if path == "files/outer.arc/bad.bin"),
            "{err}"
        );
    }
}
