//! Identifying a blob of bytes by its magic and handing it to that format's decoder.
//!
//! Detection is content-only: some files on the retail disc carry a path or
//! extension that doesn't match what's inside, so nothing here branches on a
//! name, only on checking each format's magic. A blob nothing recognises
//! passes through unchanged, whatever its name claims.
//!
//! Yaz0 is peeled before content is sniffed, since most files on disc arrive
//! wrapped and nothing downstream expects to see it. Whether a wrapper was
//! peeled is carried along and recorded, so the fact survives the round trip.

use tpmt_arc::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_bmg::editable::json;
use tpmt_compress::{is_yaz0, yaz0_decode};

use crate::{Error, Result};

/// What went wrong decoding one file, without where. [`Error::Decode`] adds
/// the path, attached here at the innermost point so a member of a nested
/// archive names itself rather than the disc file it came in.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Archive(#[from] tpmt_arc::Error),

    #[error(transparent)]
    Message(#[from] tpmt_bmg::Error),

    #[error(transparent)]
    Compress(#[from] tpmt_compress::Error),
}

/// Every `(project path, bytes)` pair decoding `data` produced, however deep
/// the recursion went to get there: one pair for a plain file, one per member
/// plus a sidecar for an archive.
///
/// The leading tag picks the format before its decoder runs, so an error out
/// of a decoder always means "this format, but broken", never "not this
/// format". A new leaf format is one more arm with its tag.
pub fn decode(path: &str, data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let unwrapped = peel(data).map_err(at(path))?;
    let sniffed = unwrapped.as_deref().unwrap_or(data);

    // Every format announces itself with a four-byte ASCII tag, so that is
    // the key. The decoder still checks whatever follows the tag.
    match sniffed.first_chunk() {
        Some(b"RARC") => {
            let archive = tpmt_arc::unpack(sniffed).map_err(at(path))?;
            decode_archive(path, &archive, unwrapped.is_some())
        }
        // An editable leaf has nowhere to record a wrapper, so a wrapped one
        // stays as it arrived. Members are already bare here; only a loose
        // disc file can hit the guard.
        Some(b"MESG") if unwrapped.is_none() => {
            let bmg = tpmt_bmg::unpack(sniffed).map_err(at(path))?;
            let editable = json::encode(&bmg).map_err(at(path))?;
            Ok(vec![(format!("{path}.{}", json::EXTENSION), editable)])
        }
        _ => Ok(vec![(path.to_string(), data.to_vec())]),
    }
}

/// A member's wrapper, unlike a loose file's, has somewhere to be recorded:
/// its [`Member`] entry. So every member is peeled here, and `decode` below
/// only ever sees a nested archive bare. That makes [`Sidecar::yaz0_compressed`]
/// true only for an archive loose on the disc; a nested one's wrapping lives
/// on its member entry in the parent, and nowhere else.
fn decode_archive(
    path: &str,
    archive: &tpmt_arc::Archive<'_>,
    yaz0_compressed: bool,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut writes = Vec::new();
    let mut members = Vec::with_capacity(archive.files.len());

    for file in &archive.files {
        let member_path = format!("{path}/{}", file.path);
        let unwrapped = peel(file.data).map_err(at(&member_path))?;
        let member_data = unwrapped.as_deref().unwrap_or(file.data);

        writes.extend(decode(&member_path, member_data)?);
        members.push(Member {
            path: file.path.clone(),
            preload: file.preload,
            yaz0_compressed: unwrapped.is_some(),
            id: file.id,
        });
    }

    let sidecar = Sidecar::new(archive.root.clone(), yaz0_compressed, members);
    let toml = sidecar.to_toml().map_err(at(path))?;
    writes.push((format!("{path}/{SIDECAR}"), toml.into_bytes()));

    Ok(writes)
}

/// `data` with its Yaz0 wrapper off, or `None` if it had none.
fn peel(data: &[u8]) -> Result<Option<Vec<u8>>, tpmt_compress::Error> {
    is_yaz0(data).then(|| yaz0_decode(data)).transpose()
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

    use tpmt_arc::{Archive, File};
    use tpmt_compress::yaz0_encode;

    use super::*;

    fn wrap(data: &[u8]) -> Vec<u8> {
        yaz0_encode(data, false).unwrap()
    }

    fn archive(root: &str, files: Vec<File<'_>>) -> Vec<u8> {
        tpmt_arc::pack(&Archive {
            root: root.to_string(),
            files,
            next_free_id: None,
        })
        .unwrap()
    }

    fn file<'a>(path: &str, data: &'a [u8]) -> File<'a> {
        File {
            path: path.to_string(),
            data,
            ..Default::default()
        }
    }

    fn sidecar(outputs: &BTreeMap<String, Vec<u8>>, dir: &str) -> Sidecar {
        let bytes = &outputs[&format!("{dir}/{SIDECAR}")];
        Sidecar::from_toml(std::str::from_utf8(bytes).unwrap()).unwrap()
    }

    /// A blob nothing recognises comes through as it was, wrapper included:
    /// a loose file has nowhere to record that it was wrapped.
    #[test]
    fn unrecognised_bytes_pass_through_still_wrapped() {
        let wrapped = wrap(b"not a format");
        let outputs = decode("files/thing.bin", &wrapped).unwrap();
        assert_eq!(outputs, vec![("files/thing.bin".to_string(), wrapped)]);
    }

    /// A wrapped archive holding a wrapped member and a wrapped nested
    /// archive. Every wrapper comes off; each is recorded exactly once.
    #[test]
    fn nested_archive_wrapping_is_recorded_on_the_member_only() {
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

        let outputs: BTreeMap<_, _> = decode("files/outer.arc", &outer)
            .unwrap()
            .into_iter()
            .collect();

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
        assert!(
            outer.yaz0_compressed,
            "loose archive records its own wrapper"
        );
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

        let inner = sidecar(&outputs, "files/outer.arc/inner.arc");
        assert!(
            !inner.yaz0_compressed,
            "a nested archive's wrapper belongs to its member entry, not itself"
        );
    }

    /// The innermost path, not the disc file's, names a failure.
    #[test]
    fn errors_name_the_member_that_failed() {
        let mut truncated = wrap(b"member bytes that will be cut short");
        truncated.truncate(12);
        let outer = archive("outer", vec![file("bad.bin", &truncated)]);

        let err = decode("files/outer.arc", &outer).unwrap_err();
        assert!(
            matches!(&err, Error::Decode { path, .. } if path == "files/outer.arc/bad.bin"),
            "{err}"
        );
    }
}
