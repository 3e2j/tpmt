//! Identifying a blob of bytes by its magic and handing it to that format's decoder.
//!
//! Detection is content-only: some files on the retail disc carry a path or
//! extension that doesn't match what's inside, so nothing here branches on a
//! name, only on checking each format's magic. A blob nothing recognises
//! passes through unchanged, whatever its name claims.
//!
//! Yaz0 is peeled before content is sniffed, since most files on disc arrive
//! wrapped and nothing downstream expects to see it. Whether a wrapper came
//! off is handed back to the caller, since the record of it belongs to
//! whatever holds the file: an archive writes it on the member's sidecar
//! entry, the disc on `yaz0.toml`. A file never records its own.

use tpmt_arc::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_arc::{Archive, Format};
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

/// Every `(project path, bytes)` pair one file produces, however deep the
/// recursion went to get there: one pair for a plain file, one per member
/// plus a sidecar for an archive.
pub type Writes = Vec<(String, Vec<u8>)>;

/// One file taken apart, and whether a Yaz0 wrapper came off it first.
#[derive(Debug)]
pub struct Decoded {
    pub writes: Writes,
    /// The caller records this; see the module doc.
    pub yaz0_compressed: bool,
}

/// Peels `data`, then hands it to whichever format's magic it opens with.
///
/// Each format's magic picks it before its decoder runs, so an error out of
/// a decoder always means "this format, but broken", never "not this
/// format". A new leaf format is one more `recognises` check.
pub fn decode(path: &str, data: &[u8]) -> Result<Decoded> {
    let yaz0_compressed = is_yaz0(data);
    let unwrapped = yaz0_compressed
        .then(|| yaz0_decode(data))
        .transpose()
        .map_err(at(path))?;
    let bare = unwrapped.as_deref().unwrap_or(data);

    let writes = if Archive::recognises(bare) {
        explode(path, bare)?
    } else {
        // Translation layers (e.g. tpmt_bmg::editable::json) are deprecated
        // for now: raw game files + a UI is the scoped-down editing path. A
        // leaf format passes through untouched until that changes.
        vec![(path.to_string(), bare.to_vec())]
    };

    Ok(Decoded {
        writes,
        yaz0_compressed,
    })
}

/// Explodes the archive in `bare` into its members' `(project path, bytes)`
/// pairs, plus a [`SIDECAR`] recording each member's path, preload flag, id,
/// and Yaz0 wrapper.
fn explode(path: &str, bare: &[u8]) -> Result<Writes> {
    let archive = Archive::decode(bare).map_err(at(path))?;
    let mut writes = Vec::new();
    let mut members = Vec::with_capacity(archive.files.len());

    for file in &archive.files {
        let decoded = decode(&format!("{path}/{}", file.path), file.data)?;
        writes.extend(decoded.writes);
        members.push(Member {
            path: file.path.clone(),
            preload: file.preload,
            yaz0_compressed: decoded.yaz0_compressed,
            id: file.id,
        });
    }

    let sidecar = Sidecar::new(archive.root, members);
    let toml = sidecar.to_toml().map_err(at(path))?;
    writes.push((format!("{path}/{SIDECAR}"), toml.into_bytes()));

    Ok(writes)
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

    use tpmt_arc::File;
    use tpmt_compress::yaz0_encode;

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

    fn sidecar(outputs: &BTreeMap<String, Vec<u8>>, dir: &str) -> Sidecar {
        let bytes = &outputs[&format!("{dir}/{SIDECAR}")];
        Sidecar::from_toml(std::str::from_utf8(bytes).unwrap()).unwrap()
    }

    /// A blob nothing recognises comes through bare, and the wrapper that
    /// came off it is reported for the caller to record.
    #[test]
    fn unrecognised_bytes_pass_through_unwrapped() {
        let decoded = decode("files/thing.bin", &wrap(b"not a format")).unwrap();
        assert!(decoded.yaz0_compressed);
        assert_eq!(
            decoded.writes,
            vec![("files/thing.bin".to_string(), b"not a format".to_vec())]
        );
    }

    /// A wrapped archive holding a wrapped member and a wrapped nested
    /// archive. Every wrapper comes off; each is recorded exactly once, by
    /// whatever held the file.
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

        let decoded = decode("files/outer.arc", &outer).unwrap();
        assert!(
            decoded.yaz0_compressed,
            "a loose archive's wrapper is reported up, not written anywhere"
        );
        let outputs: BTreeMap<_, _> = decoded.writes.into_iter().collect();

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

        let err = decode("files/outer.arc", &outer).unwrap_err();
        assert!(
            matches!(&err, Error::Decode { path, .. } if path == "files/outer.arc/bad.bin"),
            "{err}"
        );
    }
}
