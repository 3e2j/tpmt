//! Putting one disc file back together, the inverse of
//! [`explode`](crate::unpack::explode).
//!
//! Builds leaves first. An archive's header records each member's offset and
//! size, so the archive can't encode until every member's bytes are final.
//! The recursion assembles each member, Yaz0-wraps it if its entry says so,
//! then encodes the archive. A disc file's own wrapper goes on last, because
//! the disc records it, not an archive.

use std::borrow::Cow;

use rayon::prelude::*;
use tpmt_archive::editable::sidecar::Sidecar;
use tpmt_archive::{Archive, File, Format};
use tpmt_compression::{Strategy, Yaz0};

use super::tree::Tree;
use crate::{Error, Result};

/// What went wrong writing one file, without where. [`Error::Encode`] adds
/// the path, attached at the innermost point so a member of a nested archive
/// names itself rather than the disc file it goes into.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error(transparent)]
    Archive(#[from] tpmt_archive::Error),

    #[error(transparent)]
    Compress(#[from] tpmt_compression::Error),
}

/// Assembles one disc file: everything under it, then the Yaz0 wrapper if the
/// disc held it wrapped.
///
/// `wrapped` comes from what the unpack recorded, since a loose file never
/// records its own wrapper.
///
/// # Errors
///
/// - whatever reading a file the archive holds returns
/// - [`Error::Sidecar`] if an archive's sidecar won't read
/// - [`Error::Encode`] if what came out does not fit the format
pub fn disc_file<E>(tree: &Tree<'_, E>, path: &str, wrapped: bool) -> Result<Vec<u8>, E>
where
    E: From<Error> + Send,
{
    let bare = node(tree, path)?;
    if wrapped {
        Ok(wrap(path, &bare)?)
    } else {
        Ok(bare)
    }
}

/// One project path's final bytes, minus whatever wrapper its container puts
/// back on it.
fn node<E>(tree: &Tree<'_, E>, path: &str) -> Result<Vec<u8>, E>
where
    E: From<Error> + Send,
{
    if tree.is_archive(path) {
        archive(tree, path)
    } else {
        tree.file(path)
    }
}

/// Every member assembled, then the archive around them.
fn archive<E>(tree: &Tree<'_, E>, path: &str) -> Result<Vec<u8>, E>
where
    E: From<Error> + Send,
{
    let sidecar = tree.sidecar(path)?;
    let members = tree.members(path, &sidecar);

    // Final bytes, in member order. Members are independent, and one archive
    // can hold most of a build's Yaz0 work, so they encode in parallel rather
    // than leaving it to the single task `rebuild` gave this disc file.
    let bytes = members
        .par_iter()
        .map(|member| {
            let inner = format!("{path}/{}", member.path);
            let assembled = node(tree, &inner)?;
            if member.yaz0_compressed {
                Ok(wrap(&inner, &assembled)?)
            } else {
                Ok(assembled)
            }
        })
        .collect::<Result<Vec<_>, E>>()?;

    // TODO: the linker goes here, where every member's bytes exist, the member
    // list is fixed, and the archive has not been encoded yet. Lands with its first user (`.stb`), as
    // a trait in tpmt-archive that a decoded file implements to hand out
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
            ..Default::default()
        })
        .collect();

    let Sidecar { root, .. } = sidecar;
    let encoded = Archive {
        root,
        files,
        next_free_id: None,
    }
    .encode()
    .map_err(at(path))?;
    Ok(encoded)
}

fn wrap(path: &str, data: &[u8]) -> Result<Vec<u8>> {
    // Forced cheap (vanilla) strategy here. May be opened up in future
    // when customization comes into play.
    Yaz0 {
        data: Cow::Borrowed(data),
        strategy: Strategy::Parity,
    }
    .encode()
    .map_err(at(path))
}

fn at<E: Into<EncodeError>>(path: &str) -> impl FnOnce(E) -> Error + '_ {
    move |source| Error::Encode {
        path: path.to_string(),
        source: source.into(),
    }
}
