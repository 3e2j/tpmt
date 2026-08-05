//! Working out what a build has to produce, which is the half both endings
//! share.
//!
//! Edits are never tracked as they happen, so this is where they are found: the
//! project tree is walked, every file in it is hashed, and anything matching
//! the hash taken at unpack is left alone. An untouched file is bytes on the
//! source disc and goes back as those bytes, never through a codec.
//!
//! An archive is one file on the disc however deep it was unpacked, so it is
//! decided as a whole. Untouched, it is copied off the disc still compressed.
//! Touched anywhere inside, it is repacked, and even then only the edited
//! members come out of the project: every other one is pulled out of the
//! original, which is the same bytes it always was.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use sha1::{Digest, Sha1};
use tpmt_disc::{Disc, Item};

use crate::Error;
use crate::project::{FILES, SYS, read};

/// One finished game file, ready to go on a disc or into a mod.
pub(crate) struct Output {
    /// What the disc knows it by: `sys/main.dol`, or somewhere under `files/`.
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) source: Source,
}

/// Where a finished file's bytes come from.
pub(crate) enum Source {
    /// Untouched, so it is copied off the source disc exactly as it was.
    Disc { offset: u64 },
    /// A file somebody edited, which is already the bytes that go on the disc.
    Project(PathBuf),
    /// An archive repacked because something inside it changed.
    ///
    /// The one thing held in memory rather than fetched when it is wanted: an
    /// archive's length is not known until it has been built, and a disc layout
    /// needs every length before it can place anything.
    Built(Vec<u8>),
}

impl Output {
    /// The bytes themselves, which is the last moment they are needed and so
    /// the first moment worth reading them.
    pub(crate) fn bytes(&self, disc: &Disc) -> Result<Cow<'_, [u8]>, Error> {
        Ok(match &self.source {
            Source::Disc { offset } => Cow::Owned(disc.read(*offset, self.size)?),
            Source::Project(path) => Cow::Owned(read(path)?),
            Source::Built(bytes) => Cow::Borrowed(bytes),
        })
    }

    /// Whether this is something a person changed, which is the whole of what a
    /// mod holds.
    pub(crate) fn is_changed(&self) -> bool {
        !matches!(self.source, Source::Disc { .. })
    }
}

/// What a build produces, in no particular order: a disc layout has an order of
/// its own, and a mod has no use for one.
pub(crate) struct Plan {
    /// Everything the disc will hold, directories included.
    pub(crate) items: Vec<Item>,
    /// Where each of those files comes from, in step with the items above.
    pub(crate) outputs: Vec<Output>,
}

/// Works out what every file on the built disc is going to be.
pub(crate) fn plan(
    project: &Path,
    disc: &Disc,
    vanilla: &HashMap<String, String>,
) -> Result<Plan, Error> {
    let mut nodes = Vec::new();
    walk(project, SYS, &mut nodes)?;
    walk(project, FILES, &mut nodes)?;

    // One flat pass over every file in the project, which is where a build
    // spends most of its reading.
    let leaves: Vec<&String> = nodes.iter().flat_map(Node::leaves).collect();
    let hashes: HashMap<&str, String> = leaves
        .par_iter()
        .map(|path| Ok((path.as_str(), hash(&read(&project.join(path))?))))
        .collect::<Result<_, Error>>()?;

    // A file nobody has a hash for is one nobody had at unpack, so it counts as
    // changed for the same reason an edited one does.
    let changed: HashSet<&str> = hashes
        .iter()
        .filter(|(path, hash)| vanilla.get(**path) != Some(hash))
        .map(|(path, _)| *path)
        .collect();

    // How many files each disc file was unpacked into, then and now. An archive
    // whose count moved had a member added or taken out, which is a change even
    // when every member it still has is untouched.
    let mut before: HashMap<&str, usize> = HashMap::new();
    for path in vanilla.keys() {
        *before.entry(owner(path)).or_default() += 1;
    }
    let mut now: HashMap<&str, usize> = HashMap::new();
    for path in &leaves {
        *now.entry(owner(path)).or_default() += 1;
    }

    let entries = disc.entries()?;
    let on_disc: HashMap<&str, (u64, u64)> = entries
        .iter()
        .filter_map(|entry| match entry {
            tpmt_disc::Entry::File { path, offset, size } => {
                Some((path.as_str(), (*offset, *size)))
            }
            tpmt_disc::Entry::Directory { .. } => None,
        })
        .collect();

    // The archives that have to be rebuilt, done first and in parallel: each
    // one is a decompress, a repack and a recompress, and there can be a lot of
    // them.
    let untouched = |node: &Node| {
        let path = node.path();
        !node
            .leaves()
            .iter()
            .any(|leaf| changed.contains(leaf.as_str()))
            && before.get(path) == now.get(path)
    };
    let mut repacked: HashMap<&str, Vec<u8>> = nodes
        .par_iter()
        .filter(|node| matches!(node, Node::Archive { .. }) && !untouched(node))
        .map(|node| {
            let path = node.path();
            let (offset, size) = *on_disc
                .get(path)
                .ok_or_else(|| Error::NewArchive(path.to_string()))?;
            let original = disc.read(offset, size)?;
            Ok((path, repack(project, path, &original, &changed)?))
        })
        .collect::<Result<_, Error>>()?;

    let mut items = Vec::with_capacity(nodes.len());
    let mut outputs = Vec::new();
    for node in &nodes {
        let path = node.path().to_string();
        if let Node::Directory { .. } = node {
            items.push(Item::Directory { path });
            continue;
        }

        // An archive is decided as a whole, and a file on its own is decided by
        // its own hash.
        let (size, source) = match repacked.remove(node.path()) {
            Some(bytes) => (bytes.len() as u64, Source::Built(bytes)),
            None if untouched(node) => {
                let (offset, size) = *on_disc
                    .get(node.path())
                    .ok_or_else(|| Error::NotOnDisc(path.clone()))?;
                (size, Source::Disc { offset })
            }
            None => {
                let at = project.join(&path);
                (length(&at)?, Source::Project(at))
            }
        };

        items.push(Item::File {
            path: path.clone(),
            size,
        });
        outputs.push(Output { path, size, source });
    }

    Ok(Plan { items, outputs })
}

/// Repacks an archive around the members that changed.
///
/// Everything that did not change is handed straight back out of the original,
/// so nothing is re-encoded because something next to it was edited. An archive
/// nested inside this one is the same job again, and only if something under it
/// changed.
///
/// Whether to compress on the way out is not recorded anywhere, and does not
/// have to be: the original is in hand here, and it says.
fn repack(
    project: &Path,
    path: &str,
    original: &[u8],
    changed: &HashSet<&str>,
) -> Result<Vec<u8>, Error> {
    let archive = |source| Error::Archive {
        path: PathBuf::from(path),
        source,
    };
    let compress = |source| Error::Compress {
        path: PathBuf::from(path),
        source,
    };

    let wrapped = tpmt_compress::is_yaz0(original);
    let raw = match wrapped {
        true => Cow::Owned(tpmt_compress::yaz0_decode(original).map_err(compress)?),
        false => Cow::Borrowed(original),
    };
    let members = tpmt_arc::files(&raw).map_err(archive)?;

    // Rebuilding the tables a changed member set would take is the re-emitter
    // still marked TODO in tpmt-arc, so an add or a remove is refused by name
    // here rather than quietly built back into the original's shape.
    let mut in_project = Vec::new();
    member_names(project, path, "", &mut in_project)?;
    let original_names: HashSet<&str> = members.iter().map(|member| member.path.as_str()).collect();
    for name in &in_project {
        if !original_names.contains(name.as_str()) {
            return Err(Error::AddedMember(format!("{path}/{name}")));
        }
    }
    let in_project: HashSet<&str> = in_project.iter().map(String::as_str).collect();
    for member in &members {
        if !in_project.contains(member.path.as_str()) {
            return Err(Error::DeletedMember(format!("{path}/{}", member.path)));
        }
    }

    // Held apart from the list below so that a member taken out of the project
    // outlives the archive being built out of it.
    let mut fresh = Vec::with_capacity(members.len());
    for member in &members {
        let at = format!("{path}/{}", member.path);
        fresh.push(match member.path.ends_with(".arc") {
            true if touched(changed, &at) => Some(repack(project, &at, member.data, changed)?),
            _ if changed.contains(at.as_str()) => Some(read(&project.join(&at))?),
            _ => None,
        });
    }

    let members: Vec<tpmt_arc::File> = members
        .iter()
        .zip(&fresh)
        .map(|(member, replacement)| tpmt_arc::File {
            path: member.path.clone(),
            data: replacement.as_deref().unwrap_or(member.data),
        })
        .collect();

    let built = tpmt_arc::build(&raw, &members).map_err(archive)?;
    match wrapped {
        true => tpmt_compress::yaz0_encode(&built).map_err(compress),
        false => Ok(built),
    }
}

/// One thing the project holds, at the path the disc knows it by.
enum Node {
    Directory {
        path: String,
    },
    File {
        path: String,
    },
    /// A directory whose name ends in `.arc`, which is an archive taken apart.
    /// Every file under it, at any depth, is part of one file on the disc.
    Archive {
        path: String,
        leaves: Vec<String>,
    },
}

impl Node {
    fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path } | Self::Archive { path, .. } => path,
        }
    }

    /// Every file in the project this covers, which is itself unless it is a
    /// directory of some kind.
    fn leaves(&self) -> &[String] {
        match self {
            Self::Directory { .. } => &[],
            Self::File { path } => std::slice::from_ref(path),
            Self::Archive { leaves, .. } => leaves,
        }
    }
}

/// Walks a directory of the project, turning it back into the entries a disc
/// holds. An `.arc` directory becomes one file again; everything else keeps its
/// shape, empty directories included.
fn walk(project: &Path, at: &str, nodes: &mut Vec<Node>) -> Result<(), Error> {
    for (path, directory) in listing(&project.join(at))? {
        let path = format!("{at}/{path}");
        match (directory, path.ends_with(".arc")) {
            (true, true) => {
                let mut leaves = Vec::new();
                gather(project, &path, &mut leaves)?;
                nodes.push(Node::Archive { path, leaves });
            }
            (true, false) => {
                nodes.push(Node::Directory { path: path.clone() });
                walk(project, &path, nodes)?;
            }
            (false, _) => nodes.push(Node::File { path }),
        }
    }
    Ok(())
}

/// The member names an archive directory holds now, in the form the archive's
/// own tables spell them. A `.arc` directory is one name however deep its
/// contents go, since it is one member; any other directory is structure
/// inside this archive and recurses.
fn member_names(
    project: &Path,
    at: &str,
    inside: &str,
    names: &mut Vec<String>,
) -> Result<(), Error> {
    for (name, directory) in listing(&project.join(at))? {
        let path = format!("{at}/{name}");
        let member = match inside.is_empty() {
            true => name.clone(),
            false => format!("{inside}/{name}"),
        };
        match directory && !name.ends_with(".arc") {
            true => member_names(project, &path, &member, names)?,
            false => names.push(member),
        }
    }
    Ok(())
}

/// Every file under a directory, at any depth. Archives nested inside archives
/// are more of the same file, not a boundary.
fn gather(project: &Path, at: &str, leaves: &mut Vec<String>) -> Result<(), Error> {
    for (path, directory) in listing(&project.join(at))? {
        let path = format!("{at}/{path}");
        match directory {
            true => gather(project, &path, leaves)?,
            false => leaves.push(path),
        }
    }
    Ok(())
}

/// What a directory holds, by name, in an order that does not depend on the
/// filesystem. Nothing downstream needs this order, but a build that is the
/// same twice is worth more than the sort costs.
fn listing(path: &Path) -> Result<Vec<(String, bool)>, Error> {
    let failed = |source| Error::Read {
        path: path.to_path_buf(),
        source,
    };

    let mut listing = Vec::new();
    for entry in fs::read_dir(path).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let directory = entry.file_type().map_err(failed)?.is_dir();
        listing.push((name, directory));
    }
    listing.sort();
    Ok(listing)
}

fn length(path: &Path) -> Result<u64, Error> {
    Ok(fs::metadata(path)
        .map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?
        .len())
}

/// The disc file a project path belongs to: itself, or the archive holding it,
/// since an archive is one file on the disc however deep it was unpacked.
fn owner(path: &str) -> &str {
    let mut at = 0;
    for part in path.split('/') {
        at += part.len();
        if part.ends_with(".arc") {
            return &path[..at];
        }
        at += 1;
    }
    path
}

/// Whether anything under a directory changed.
fn touched(changed: &HashSet<&str>, path: &str) -> bool {
    changed
        .iter()
        .any(|at| at.strip_prefix(path).is_some_and(|at| at.starts_with('/')))
}

/// What change detection compares. Taken at unpack over the bytes as they were
/// written into the project, and again at build over what is there now.
pub(crate) fn hash(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}
