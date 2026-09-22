//! Putting one disc file back together, the inverse of
//! [`explode`](crate::unpack::explode).
//!
//! Builds leaves first. An archive's header records each member's offset and
//! size, so the archive can't encode until every member's bytes are final.
//! The recursion assembles each member, Yaz0-wraps it if its entry says so,
//! then encodes the archive. A disc file's own wrapper goes on last, because
//! the disc records it, not an archive.

use tpmt_jkernel_arc::editable::sidecar::Sidecar;
use tpmt_jkernel_arc::{Archive, File, Format};
use tpmt_jkernel_compress::yaz0_encode;

use super::tree::Tree;
use crate::{Error, Result};

/// What went wrong writing one file, without where. [`Error::Encode`] adds
/// the path, attached at the innermost point so a member of a nested archive
/// names itself rather than the disc file it goes into.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error(transparent)]
    Archive(#[from] tpmt_jkernel_arc::Error),

    #[error(transparent)]
    Compress(#[from] tpmt_jkernel_compress::Error),
}

/// Assembles one disc file: everything under it, then the Yaz0 wrapper if the
/// disc held it wrapped.
///
/// `wrapped` comes from `base/yaz0.toml`, since a loose file never records
/// its own wrapper.
///
/// # Errors
///
/// - [`Error::MissingFile`] if something the archive holds is in neither layer
/// - [`Error::BaseModified`] if a vanilla file no longer hashes to what it did
/// - [`Error::Encode`] if what came out does not fit the format
pub fn disc_file(tree: &Tree, path: &str, wrapped: bool) -> Result<Vec<u8>> {
    let bare = node(tree, path)?;
    if wrapped { wrap(path, &bare) } else { Ok(bare) }
}

/// One project path's final bytes, minus whatever wrapper its container puts
/// back on it.
fn node(tree: &Tree, path: &str) -> Result<Vec<u8>> {
    if tree.is_archive(path) {
        archive(tree, path)
    } else {
        tree.file(path)
    }
}

/// Every member assembled, then the archive around them.
fn archive(tree: &Tree, path: &str) -> Result<Vec<u8>> {
    let sidecar = tree.sidecar(path)?;
    let members = tree.members(path, &sidecar);

    // Final bytes, in member order.
    let mut bytes = Vec::with_capacity(members.len());
    for member in &members {
        let inner = format!("{path}/{}", member.path);
        let assembled = node(tree, &inner)?;
        bytes.push(if member.yaz0_compressed {
            wrap(&inner, &assembled)?
        } else {
            assembled
        });
    }

    // TODO: the linker goes here, where every member's bytes exist, the member
    // list is fixed, and the archive has not been encoded yet. Lands with its first user (`.stb`), as
    // a trait in tpmt-jkernel-arc that a decoded file implements to hand out
    // `&mut` to every reference it holds, each an enum of bare `Id(u16)` or
    // resolved `Path(String)`. Unpack turns `Id`s into `Path`s via the
    // archive's id -> path map (free from `Sidecar::members`). Build turns
    // `Path`s back into ids here. There is no cycle, since id assignment never
    // depends on referencer content. References never leave their archive
    // (`JKRArchive::getResource`/`findIdResource` in the decomp), so the
    // linker runs per archive.
    let files = members
        .iter()
        .zip(&bytes)
        .map(|(member, data)| File {
            path: member.path.clone(),
            data,
            id: member.id,
            preload: member.preload,
        })
        .collect();

    let Sidecar { root, .. } = sidecar;
    Archive {
        root,
        files,
        next_free_id: None,
    }
    .encode()
    .map_err(at(path))
}

fn wrap(path: &str, data: &[u8]) -> Result<Vec<u8>> {
    // Forced cheap (vanilla) strategy here. May be opened up in future
    // when customization comes into play.
    yaz0_encode(data, false).map_err(at(path))
}

fn at<E: Into<EncodeError>>(path: &str) -> impl FnOnce(E) -> Error + '_ {
    move |source| Error::Encode {
        path: path.to_string(),
        source: source.into(),
    }
}
