//! Working out what a build has to produce, which is the half both endings
//! share.
//!
//! Edits are never tracked as they happen, so this is where they are found: the
//! project tree is walked, every file in it is hashed, and anything matching
//! the hash taken at unpack is left alone. An untouched file is bytes on the
//! source disc and goes back as those bytes, never through a codec.
//!
//! A file the project no longer holds has nothing to hash and nothing to
//! walk, so it simply never enters the output: a deletion stays a deletion
//! rather than falling back to the vanilla bytes it once matched. That holds
//! whether it is a top-level file or an archive member.
//!
//! An archive is one file on the disc however deep it was unpacked, decided
//! as a whole the way the crate root describes. Untouched, it is copied off
//! the disc unopened. Touched anywhere, every member in it is rebuilt from
//! the project, not only the ones that changed: a linker can make one
//! member's bytes depend on another's, so there is no such thing as a member
//! an edit elsewhere cannot reach.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use sha1::{Digest, Sha1};
use tpmt_arc::Preload;
use tpmt_arc::editable::sidecar::{Member, SIDECAR, Sidecar};
use tpmt_disc::{Disc, Item};

use crate::Error;
use crate::fs::read;
use crate::manifest::{FILES, SYS};
use crate::sidecars::arc::read_sidecar;

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
    let (nodes, hashes) = hashed_leaves(project)?;
    let leaves: Vec<&String> = nodes.iter().flat_map(Node::leaves).collect();

    let entries = disc.entries()?;
    let on_disc = crate::on_disc(&entries);
    let baseline = Baseline::build(vanilla, &hashes, &leaves, on_disc);

    // The archives that have to be rebuilt, done first and in parallel: each
    // one is a decompress, a repack and a recompress, and there can be a lot
    // of them.
    let mut repacked = repack_changed(project, &nodes, &baseline)?;

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
            None if baseline.is_untouched(node) => {
                let (offset, size) = *baseline
                    .on_disc
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

/// Walks the project tree and hashes every leaf in parallel. Shared first
/// step for `plan` and `changes`.
fn hashed_leaves(project: &Path) -> Result<(Vec<Node>, HashMap<String, String>), Error> {
    let mut nodes = Vec::new();
    walk(project, SYS, &mut nodes)?;
    walk(project, FILES, &mut nodes)?;

    // One flat pass over every file in the project, which is where a build
    // spends most of its reading.
    let leaves: Vec<&String> = nodes.iter().flat_map(Node::leaves).collect();
    let hashes: HashMap<String, String> = leaves
        .par_iter()
        .map(|path| Ok(((*path).clone(), hash(&read(&project.join(path))?))))
        .collect::<Result<_, Error>>()?;

    Ok((nodes, hashes))
}

/// What tells `plan` a node can be copied off the source disc unopened,
/// gathered once rather than re-threaded through every helper that needs a
/// piece of it.
struct Baseline<'a> {
    /// A file nobody has a hash for is one nobody had at unpack, so it counts
    /// as changed for the same reason an edited one does. So does a file the
    /// project no longer holds: a removal has no bytes to hash, but the
    /// archive that used to hold the member still has to be rebuilt without
    /// it.
    changed: HashSet<&'a str>,
    /// How many files each disc file was unpacked into, before and now. An
    /// archive whose count moved had a member added or taken out, which is a
    /// change even when every member it still has is untouched.
    before: HashMap<&'a str, usize>,
    now: HashMap<&'a str, usize>,
    on_disc: HashMap<&'a str, (u64, u64)>,
}

impl<'a> Baseline<'a> {
    fn build(
        vanilla: &'a HashMap<String, String>,
        hashes: &'a HashMap<String, String>,
        leaves: &'a [&'a String],
        on_disc: HashMap<&'a str, (u64, u64)>,
    ) -> Self {
        let mut changed: HashSet<&str> = hashes
            .iter()
            .filter(|(path, digest)| vanilla.get(path.as_str()) != Some(*digest))
            .map(|(path, _)| path.as_str())
            .collect();
        changed.extend(
            vanilla
                .keys()
                .filter(|path| !hashes.contains_key(path.as_str()))
                .map(String::as_str),
        );

        let mut before: HashMap<&str, usize> = HashMap::new();
        for path in vanilla.keys() {
            *before.entry(owner(path)).or_default() += 1;
        }
        let mut now: HashMap<&str, usize> = HashMap::new();
        for path in leaves {
            *now.entry(owner(path)).or_default() += 1;
        }

        Baseline {
            changed,
            before,
            now,
            on_disc,
        }
    }

    /// Untouched is a claim that the source disc has the bytes already, so
    /// something the disc never had is never untouched, however little is in
    /// it.
    fn is_untouched(&self, node: &Node) -> bool {
        let path = node.path();
        !node
            .leaves()
            .iter()
            .any(|leaf| self.changed.contains(leaf.as_str()))
            && self.before.get(path) == self.now.get(path)
            && self.on_disc.contains_key(path)
    }
}

/// Rebuilds every archive the baseline says changed, in parallel: each one is
/// a decompress, a repack and a recompress, and there can be a lot of them.
fn repack_changed<'a>(
    project: &Path,
    nodes: &'a [Node],
    baseline: &Baseline,
) -> Result<HashMap<&'a str, Vec<u8>>, Error> {
    nodes
        .par_iter()
        .filter(|node| matches!(node, Node::Archive { .. }) && !baseline.is_untouched(node))
        .map(|node| {
            let path = node.path();
            Ok((
                path,
                repack(project, path, baseline.on_disc.contains_key(path))?,
            ))
        })
        .collect()
}

/// One project leaf, and how it differs from the vanilla hash taken at
/// unpack.
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy)]
pub enum ChangeKind {
    /// The project holds this file, but the disc never did.
    Added,
    /// The project's bytes no longer match what came off the disc.
    Modified,
    /// The disc held this, and the project no longer does.
    Deleted,
}

/// Every leaf that differs from vanilla, sorted by path.
///
/// The same walk-and-hash `plan` opens with, without the disc: a status has
/// nothing to read off it and nothing to repack, since all it needs is which
/// files changed, not what a build would produce from them.
pub(crate) fn changes(
    project: &Path,
    vanilla: &HashMap<String, String>,
) -> Result<Vec<Change>, Error> {
    let (_, hashes) = hashed_leaves(project)?;

    let mut changes: Vec<Change> = hashes
        .iter()
        .filter_map(|(path, digest)| match vanilla.get(path.as_str()) {
            Some(vanilla_digest) if vanilla_digest == digest => None,
            Some(_) => Some(Change {
                path: path.to_string(),
                kind: ChangeKind::Modified,
            }),
            None => Some(Change {
                path: path.to_string(),
                kind: ChangeKind::Added,
            }),
        })
        .collect();
    changes.extend(
        vanilla
            .keys()
            .filter(|path| !hashes.contains_key(path.as_str()))
            .map(|path| Change {
                path: path.clone(),
                kind: ChangeKind::Deleted,
            }),
    );

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Rebuilds an archive out of what the project holds now.
///
/// The sidecar at the root of the directory is what says which archive this
/// is. Its root name, the order its members go back in, and everything about a
/// member that the bytes on disk cannot carry all come from there, so an
/// archive somebody wrote from nothing rebuilds the same way one off the disc
/// does.
///
/// Every member is read fresh from the project, never carried over from the
/// archive's own original bytes. Once a linker is in the picture, a member
/// nobody touched can still resolve to different bytes because something
/// elsewhere in the archive moved, so an archive being rebuilt at all is
/// rebuilt whole.
///
/// A member gone from the project drops out; one the project grew goes in at
/// the end of the main memory run, which is where a member with nothing saying
/// otherwise belongs. Nobody adding a file has to know any of this exists.
///
/// An empty directory under an archive is dropped without a word: a member
/// list cannot spell one, and the game would find nothing in it anyway.
///
/// Assembled here, not in `tpmt-arc`, for the same reason `unpack_archive`
/// builds its sidecar here: it takes walking every format crate to know a
/// member's bytes, a view `tpmt-arc` doesn't have.
fn repack(project: &Path, path: &str, existed: bool) -> Result<Vec<u8>, Error> {
    let archive = |source| Error::Archive {
        path: PathBuf::from(path),
        source,
    };
    let directory = project.join(path);

    // On an archive the disc never had there is nothing to recover, so a
    // missing sidecar is answered rather than asked for. On one the disc did
    // have, the same missing file is a lost sidecar and worth stopping over.
    let sidecar = match read_sidecar(&directory) {
        Err(Error::LostSidecar(_)) if !existed => Sidecar::fresh(),
        sidecar => sidecar?,
    };

    let mut in_project = Vec::new();
    member_names(project, path, "", &mut in_project)?;
    let (members, named) = merge_members(&sidecar, &in_project);
    let data = read_member_bytes(project, path, &directory, &members, existed, &named)?;

    // Pairs each member's metadata (from the sidecar, or filled in for a
    // member just added) with the bytes just fetched for it, in the shape the
    // packer below expects.
    let files: Vec<tpmt_arc::File> = members
        .iter()
        .zip(&data)
        .map(|(member, bytes)| tpmt_arc::File {
            path: member.path.clone(),
            data: bytes.as_slice(),
            id: member.id,
            preload: member.preload,
        })
        .collect();

    // The next-free-id counter is worked out rather than kept in the sidecar.
    // The few archives that stored a different one only differ in a field
    // nothing reads, and only once rebuilt, which has moved every offset in
    // them anyway.
    let built = tpmt_arc::pack(&tpmt_arc::Archive {
        root: sidecar.root.clone(),
        files,
        ..Default::default()
    })
    .map_err(archive)?;
    match sidecar.yaz0_compressed {
        true => crate::compress(&built, Path::new(path)),
        false => Ok(built),
    }
}

/// Reconciles the sidecar's member list against what the project holds now:
/// dropped files are left out, new ones are added. Order matches the
/// sidecar's, since that's the order the archive's bytes were laid out in.
/// `named` is which paths the sidecar already listed, for the recursive
/// nested-archive call.
fn merge_members<'s>(
    sidecar: &'s Sidecar,
    in_project: &[String],
) -> (Vec<Member>, HashSet<&'s str>) {
    let present: HashSet<&str> = in_project.iter().map(String::as_str).collect();
    let named: HashSet<&str> = sidecar
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();

    let mut members: Vec<Member> = sidecar
        .members
        .iter()
        .filter(|member| present.contains(member.path.as_str()))
        .cloned()
        .collect();
    let added: Vec<Member> = in_project
        .iter()
        .filter(|name| !named.contains(name.as_str()))
        .map(|name| Member::new(name.clone()))
        .collect();

    let at = members
        .iter()
        .position(|member| member.preload != Preload::Mram)
        .unwrap_or(members.len());
    members.splice(at..at, added);

    (members, named)
}

/// Fetches each member's actual bytes from the project: `members` only
/// carries metadata, whether read from the sidecar or filled in for a member
/// just added, never the bytes themselves.
fn read_member_bytes(
    project: &Path,
    path: &str,
    directory: &Path,
    members: &[Member],
    existed: bool,
    named: &HashSet<&str>,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut data = Vec::with_capacity(members.len());
    for member in members {
        let at = format!("{path}/{}", member.path);
        let inside = directory.join(&member.path);

        // A member unpacked into a directory is a nested archive. One that is
        // still a file is bytes however it is named, since a `.arc` the archive
        // crate could not open was never taken apart.
        let bytes = match inside.is_dir() {
            true => repack(
                project,
                &at,
                existed && named.contains(member.path.as_str()),
            )?,
            // The wrapper the sidecar recorded during unpack goes back on here.
            false => match member.yaz0_compressed {
                true => crate::compress(&read(&inside)?, Path::new(path))?,
                false => read(&inside)?,
            },
        };
        data.push(bytes);
    }
    Ok(data)
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
        match (directory, crate::is_archive(&path)) {
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
/// own entries spell them. A `.arc` directory is one name however deep its
/// contents go, since it is one member; any other directory is structure
/// inside this archive and recurses.
///
/// The sidecar is not one of them. It sits at the root describing the archive
/// rather than in it, and packing it would put a file on the disc that the game
/// never had.
fn member_names(
    project: &Path,
    at: &str,
    inside: &str,
    names: &mut Vec<String>,
) -> Result<(), Error> {
    for (name, directory) in listing(&project.join(at))? {
        if inside.is_empty() && name == SIDECAR {
            continue;
        }
        let path = format!("{at}/{name}");
        let member = match inside.is_empty() {
            true => name.clone(),
            false => format!("{inside}/{name}"),
        };
        match directory && !crate::is_archive(&name) {
            true => member_names(project, &path, &member, names)?,
            false => names.push(member),
        }
    }
    Ok(())
}

/// Every file under a directory, at any depth. Archives nested inside archives
/// are more of the same file, not a boundary.
///
/// Sidecars are gathered along with everything else. They are not members, but
/// editing one changes what the archive rebuilds into just as editing a member
/// does, so change detection has to see them.
pub(crate) fn gather(project: &Path, at: &str, leaves: &mut Vec<String>) -> Result<(), Error> {
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
pub(crate) fn owner(path: &str) -> &str {
    let mut at = 0;
    for part in path.split('/') {
        at += part.len();
        if crate::is_archive(part) {
            return &path[..at];
        }
        at += 1;
    }
    path
}

/// What change detection compares. Taken at unpack over the bytes as they were
/// written into the project, and again at build over what is there now.
pub(crate) fn hash(bytes: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::write;
    use crate::sidecars::arc::write_sidecar;
    use crate::test_support::Scratch;

    const AT: &str = "files/thing.arc";

    fn packed(files: &[(&str, &[u8], Preload)]) -> Vec<u8> {
        tpmt_arc::pack(&tpmt_arc::Archive {
            root: "archive".to_string(),
            files: files
                .iter()
                .map(|(path, data, preload)| tpmt_arc::File {
                    path: (*path).to_string(),
                    data,
                    id: None,
                    preload: *preload,
                })
                .collect(),
            ..Default::default()
        })
        .unwrap()
    }

    /// Unpacks into the scratch directory and hands back where the project is.
    fn unpacked(scratch: &Scratch, original: &[u8]) -> PathBuf {
        let mut hashes = Vec::new();
        crate::unpack_archive(original, &scratch.0.join(AT), AT, &mut hashes).unwrap();
        scratch.0.clone()
    }

    fn member<'a>(archive: &'a tpmt_arc::Archive, path: &str) -> &'a tpmt_arc::File<'a> {
        archive
            .files
            .iter()
            .find(|file| file.path == path)
            .expect("the archive should still hold it")
    }

    /// The whole point of the sidecar: an archive rebuilds out of the project
    /// with nothing to compare against, and comes back the archive it was.
    /// Byte for byte, which is also what says the sidecar file itself was not
    /// packed in as a member.
    #[test]
    fn rebuilds_without_an_original() {
        let scratch = Scratch::new("rebuilds");
        // The ARAM one goes last in the walk rather than last in the list: the
        // data section is laid out a directory at a time, so a member of the
        // root would come before anything under `sub`.
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("sub/b.bin", b"second", Preload::Mram),
            ("sub/late.bin", b"third", Preload::Aram),
        ]);

        let project = unpacked(&scratch, &original);
        let built = repack(&project, AT, false).unwrap();
        assert_eq!(built, original);
    }

    /// Editing a member used to be where an ARAM one quietly became a main
    /// memory one, since nothing outside the archive said which it was.
    #[test]
    fn an_edited_member_keeps_its_memory() {
        let scratch = Scratch::new("memory");
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("late.bin", b"third", Preload::Aram),
        ]);

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("late.bin"), b"edited").unwrap();

        let built = repack(&project, AT, false).unwrap();

        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(rebuilt.root, "archive");
        assert_eq!(member(&rebuilt, "late.bin").data, b"edited");
        assert_eq!(member(&rebuilt, "late.bin").preload, Preload::Aram);
    }

    /// A wrapped member is written out as what it is, so that whatever edits it
    /// sees the bytes rather than the wrapping, and is wrapped again on the way
    /// back in.
    #[test]
    fn a_members_wrapper_comes_off_and_goes_back_on() {
        let scratch = Scratch::new("wrapper");
        let plain = b"a run of bytes with enough repetition to encode".repeat(4);
        let wrapped = tpmt_compress::yaz0_encode(&plain, false).unwrap();
        let original = packed(&[("data.bin", &wrapped, Preload::Mram)]);

        let project = unpacked(&scratch, &original);
        let on_disk = read(&project.join(AT).join("data.bin")).unwrap();
        assert_eq!(on_disk, plain);

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        let data = member(&rebuilt, "data.bin").data;
        assert!(tpmt_compress::is_yaz0(data));
        assert_eq!(tpmt_compress::yaz0_decode(data).unwrap(), plain);
    }

    /// A nested archive is its own sidecar and its own rebuild, wrapper
    /// included, so the one holding it does not have to know what it is.
    #[test]
    fn a_nested_archive_rebuilds_itself() {
        let scratch = Scratch::new("nested");
        let inner =
            tpmt_compress::yaz0_encode(&packed(&[("in.bin", b"inner", Preload::Mram)]), false)
                .unwrap();
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("nested.arc", &inner, Preload::Mram),
        ]);

        let project = unpacked(&scratch, &original);
        assert!(project.join(AT).join("nested.arc").is_dir());

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        let nested = member(&rebuilt, "nested.arc").data;
        assert!(tpmt_compress::is_yaz0(nested));

        let opened = tpmt_compress::yaz0_decode(nested).unwrap();
        let opened = tpmt_arc::unpack(&opened).unwrap();
        assert_eq!(member(&opened, "in.bin").data, b"inner");
    }

    /// A file the sidecar never named goes on the end in main memory, which is
    /// what a member with nothing saying otherwise gets.
    #[test]
    fn an_added_member_lands_in_main_memory() {
        let scratch = Scratch::new("added");
        let original = packed(&[("a.bin", b"first", Preload::Mram)]);

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("new.bin"), b"new").unwrap();

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        let added = rebuilt.files.last().unwrap();
        assert_eq!(added.path, "new.bin");
        assert_eq!(added.data, b"new");
        assert_eq!(added.preload, Preload::Mram);
    }

    /// Main memory is a run rather than a flag, so a member with nothing saying
    /// otherwise has to land before the audio run starts, not merely be labelled
    /// main. Nobody dropping a file in should have to know either thing.
    #[test]
    fn an_added_member_lands_before_the_aram_run() {
        let scratch = Scratch::new("addedaram");
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("late.bin", b"third", Preload::Aram),
        ]);

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("new.bin"), b"new").unwrap();

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(member(&rebuilt, "new.bin").data, b"new");
        assert_eq!(member(&rebuilt, "new.bin").preload, Preload::Mram);
        assert_eq!(member(&rebuilt, "late.bin").preload, Preload::Aram);
        assert_eq!(
            rebuilt
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["a.bin", "new.bin", "late.bin"]
        );
    }

    /// A member the sidecar does name is placed by what it says, so somebody who
    /// wants their added file in audio memory says so once and gets it.
    #[test]
    fn a_member_the_sidecar_puts_in_aram_lands_there() {
        let scratch = Scratch::new("addedsaid");
        let original = packed(&[("a.bin", b"first", Preload::Mram)]);

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("new.bin"), b"new").unwrap();

        let mut sidecar = read_sidecar(&project.join(AT)).unwrap();
        sidecar.members.push(Member {
            path: "new.bin".to_string(),
            preload: Preload::Aram,
            yaz0_compressed: false,
            id: None,
        });
        write_sidecar(&sidecar, &project.join(AT)).unwrap();

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(member(&rebuilt, "new.bin").preload, tpmt_arc::Preload::Aram);
        assert_eq!(rebuilt.files.last().unwrap().path, "new.bin");
    }

    /// An archive that stored ids of its own keeps them, so a member added to
    /// one cannot be given an entry index and hope: the ids sat still while the
    /// indices moved along under them. It claims the lowest id nothing else
    /// holds instead, which here is the 0 the originals skipped.
    #[test]
    fn an_added_member_takes_an_id_nothing_else_holds() {
        let scratch = Scratch::new("addedid");
        let original = tpmt_arc::pack(&tpmt_arc::Archive {
            root: "archive".to_string(),
            files: vec![
                tpmt_arc::File {
                    path: "a.bin".to_string(),
                    data: b"first",
                    id: Some(7),
                    preload: Preload::Mram,
                },
                tpmt_arc::File {
                    path: "b.bin".to_string(),
                    data: b"second",
                    id: Some(1),
                    preload: Preload::Mram,
                },
            ],
            ..Default::default()
        })
        .unwrap();

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("new.bin"), b"new").unwrap();

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();

        let mut ids: Vec<Option<u16>> = rebuilt.files.iter().map(|file| file.id).collect();
        assert_eq!(member(&rebuilt, "new.bin").id, Some(0));
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), rebuilt.files.len());
    }

    /// Stored ids are pinned to their members, not to their places in the
    /// entry order, and adding a member leaves them where they were.
    ///
    /// `sub/b.bin` is the one that shows it. Its entry moves along to make room
    /// for the added member, so an id worked out from the order would hand
    /// anything holding the old number a different file. The stored 4 stays
    /// put, and the added member fills a gap below it instead.
    #[test]
    fn an_added_member_moves_no_id_but_its_own() {
        let scratch = Scratch::new("instep");
        let original = tpmt_arc::pack(&tpmt_arc::Archive {
            root: "archive".to_string(),
            files: vec![
                tpmt_arc::File {
                    path: "a.bin".to_string(),
                    data: b"first",
                    id: Some(0),
                    preload: Preload::Mram,
                },
                tpmt_arc::File {
                    path: "sub/b.bin".to_string(),
                    data: b"second",
                    id: Some(4),
                    preload: Preload::Mram,
                },
            ],
            ..Default::default()
        })
        .unwrap();

        let project = unpacked(&scratch, &original);
        let sidecar = read_sidecar(&project.join(AT)).unwrap();
        let ids: Vec<Option<u16>> = sidecar.members.iter().map(|member| member.id).collect();
        assert_eq!(ids, [Some(0), Some(4)]);

        write(&project.join(AT).join("new.bin"), b"new").unwrap();
        let built = repack(&project, AT, false).unwrap();

        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(rebuilt.files.len(), 3);
        assert_eq!(member(&rebuilt, "a.bin").id, Some(0));
        assert_eq!(member(&rebuilt, "sub/b.bin").id, Some(4));
        assert_eq!(member(&rebuilt, "new.bin").id, Some(1));
    }

    /// A member the sidecar names and the project no longer holds is a file
    /// somebody deleted, and deleting a file is all they should have to do.
    #[test]
    fn a_removed_member_drops_out() {
        let scratch = Scratch::new("removed");
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("sub/b.bin", b"second", Preload::Mram),
        ]);

        let project = unpacked(&scratch, &original);
        fs::remove_file(project.join(AT).join("sub").join("b.bin")).unwrap();

        let built = repack(&project, AT, true).unwrap();

        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(rebuilt.files.len(), 1);
        assert_eq!(rebuilt.files[0].path, "a.bin");
    }

    /// Deleting a member closes the gap it left in the entry order, so the same
    /// pinning that survives an addition has to survive a removal. The two after
    /// it keep the ids they were stored under rather than sliding down to 0 and
    /// 1 behind it.
    #[test]
    fn a_removed_member_moves_no_id_after_it() {
        let scratch = Scratch::new("removedids");
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("b.bin", b"second", Preload::Mram),
            ("c.bin", b"third", Preload::Mram),
        ]);

        let project = unpacked(&scratch, &original);
        fs::remove_file(project.join(AT).join("a.bin")).unwrap();

        let built = repack(&project, AT, true).unwrap();

        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(rebuilt.files.len(), 2);
        assert_eq!(member(&rebuilt, "b.bin").id, Some(1));
        assert_eq!(member(&rebuilt, "c.bin").id, Some(2));
    }

    /// The same, one directory down, and into a directory nobody had before: the
    /// run is cut from the whole layout rather than per directory, so a new
    /// directory has to land ahead of the audio one as much as a new file does.
    #[test]
    fn an_added_member_lands_in_a_subdirectory() {
        let scratch = Scratch::new("addedsub");
        let original = packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("sub/b.bin", b"second", Preload::Mram),
            ("snd/late.bin", b"third", Preload::Aram),
        ]);

        let project = unpacked(&scratch, &original);
        write(&project.join(AT).join("sub").join("new.bin"), b"new").unwrap();
        write(&project.join(AT).join("fresh").join("new.bin"), b"fresh").unwrap();

        let built = repack(&project, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(member(&rebuilt, "sub/new.bin").data, b"new");
        assert_eq!(member(&rebuilt, "fresh/new.bin").data, b"fresh");
        assert_eq!(member(&rebuilt, "snd/late.bin").preload, Preload::Aram);
        assert_eq!(rebuilt.files.last().unwrap().path, "snd/late.bin");
    }

    /// Making an archive is making a directory and calling it `.arc`. Nothing
    /// was lost taking one apart that never existed, so nothing has to be
    /// written down before it builds.
    #[test]
    fn a_new_archive_needs_no_sidecar() {
        let scratch = Scratch::new("nosidecar");
        write(&scratch.0.join(AT).join("a.bin"), b"first").unwrap();
        write(&scratch.0.join(AT).join("sub").join("b.bin"), b"second").unwrap();

        let built = repack(&scratch.0, AT, false).unwrap();
        let rebuilt = tpmt_arc::unpack(&built).unwrap();
        assert_eq!(member(&rebuilt, "a.bin").data, b"first");
        assert_eq!(member(&rebuilt, "sub/b.bin").data, b"second");
        assert!(
            rebuilt
                .files
                .iter()
                .all(|file| file.preload == Preload::Mram)
        );
    }

    /// An archive the disc did have is a different story: the sidecar is the
    /// only thing that knows what its members were loaded into and what came off
    /// it, so a missing one there is a lost one and worth stopping over.
    #[test]
    fn a_lost_sidecar_is_refused() {
        let scratch = Scratch::new("lostsidecar");
        let original = packed(&[("a.bin", b"first", Preload::Mram)]);

        let project = unpacked(&scratch, &original);
        fs::remove_file(project.join(AT).join(SIDECAR)).unwrap();

        let refused = repack(&project, AT, true);
        assert!(matches!(refused, Err(Error::LostSidecar(_))));
    }

    /// A member list cannot spell an empty directory, so one made under an
    /// archive is dropped and the rebuild comes out as though it were never
    /// there.
    #[test]
    fn an_empty_directory_under_an_archive_is_dropped() {
        let scratch = Scratch::new("emptydir");
        let original = packed(&[("a.bin", b"first", Preload::Mram)]);

        let project = unpacked(&scratch, &original);
        fs::create_dir(project.join(AT).join("empty")).unwrap();

        let built = repack(&project, AT, false).unwrap();
        assert_eq!(built, original);
    }

    /// An archive is one file on the disc however deep it was unpacked, so
    /// everything under the outermost `.arc` maps back to it, nested archives
    /// included, and anything else maps to itself.
    #[test]
    fn a_path_belongs_to_its_outermost_archive() {
        assert_eq!(owner("sys/main.dol"), "sys/main.dol");
        assert_eq!(owner("files/plain.bin"), "files/plain.bin");
        assert_eq!(owner("files/thing.arc"), "files/thing.arc");
        assert_eq!(owner("files/thing.arc/sub/a.bin"), "files/thing.arc");
        assert_eq!(
            owner("files/thing.arc/nested.arc/in.bin"),
            "files/thing.arc"
        );
    }

    // Everything below tests `plan` itself, which needs a source disc to
    // compare the project against. The fixture writes a real image through
    // tpmt-disc's own writer and unpacks it the way a project is actually
    // made, so the vanilla hashes and the disc offsets are the genuine ones.

    use tpmt_disc::{Bi2, Boot, Entry, Layout, Metadata};

    const PLAIN: &str = "files/plain.bin";

    fn metadata() -> Metadata {
        Metadata {
            boot: Boot {
                id: "GZ2E".to_string(),
                maker: "01".to_string(),
                disc_number: 0,
                revision: 0,
                audio_streaming: 0,
                stream_buffer_size: 0,
                title: "test".to_string(),
            },
            bi2: Bi2 {
                simulated_memory_size: 0x0180_0000,
                debug_flag: 0,
                country: 1,
                unknown_1c: 4,
                unknown_20: 5,
                pad_spec: 6,
            },
        }
    }

    /// Writes a small but complete disc image holding the given files.
    ///
    /// An all-zero apploader header reports both of its halves as zero, and an
    /// all-zero dol header reaches no further than itself, so each is its own
    /// length and nothing more has to be crafted for the preamble.
    fn imaged(scratch: &Scratch, files: &[(&str, &[u8])]) -> PathBuf {
        let apploader = vec![0u8; 0x20];
        let dol = vec![0u8; 0x100];
        let mut project: Vec<(&str, &[u8])> =
            vec![("sys/apploader.img", &apploader), ("sys/main.dol", &dol)];
        project.extend_from_slice(files);

        let items: Vec<Item> = project
            .iter()
            .map(|(path, data)| Item::File {
                path: (*path).to_string(),
                size: data.len() as u64,
            })
            .collect();
        let layout = Layout::plan(&metadata(), &items).unwrap();

        let mut out = Vec::new();
        let mut image = layout.write(&mut out);
        for entry in layout.entries() {
            let Entry::File { path, .. } = entry else {
                continue;
            };
            let (_, data) = project
                .iter()
                .find(|(at, _)| at == path)
                .expect("the layout only holds what it was given");
            image.file(data).unwrap();
        }
        image.finish().unwrap();

        let iso = scratch.0.join("source.iso");
        fs::write(&iso, out).unwrap();
        iso
    }

    fn two_members() -> Vec<u8> {
        packed(&[
            ("a.bin", b"first", Preload::Mram),
            ("b.bin", b"second", Preload::Mram),
        ])
    }

    /// A disc holding one plain file and one archive, unpacked into a project:
    /// what every test of `plan` starts from, before its own edit.
    fn on_disc(scratch: &Scratch) -> (PathBuf, Disc, HashMap<String, String>) {
        let archive = two_members();
        let iso = imaged(scratch, &[(PLAIN, b"plain"), (AT, &archive)]);

        let project = scratch.0.join("project");
        crate::unpack(&iso, &project, false).unwrap();
        let disc = Disc::open(&iso).unwrap();
        let vanilla = crate::store::Store::new(&project).hashes().unwrap();
        (project, disc, vanilla)
    }

    fn output<'a>(plan: &'a Plan, path: &str) -> &'a Output {
        plan.outputs
            .iter()
            .find(|output| output.path == path)
            .expect("the plan should hold it")
    }

    /// Nothing edited means nothing rebuilt: every file is a range of the
    /// source disc, the archive stays unopened, and what those ranges read
    /// back as is the bytes that went on.
    #[test]
    fn an_untouched_project_copies_everything_off_the_disc() {
        let scratch = Scratch::new("plancopies");
        let (project, disc, vanilla) = on_disc(&scratch);

        let plan = plan(&project, &disc, &vanilla).unwrap();
        assert_eq!(plan.outputs.len(), 4);
        assert!(plan.outputs.iter().all(|output| !output.is_changed()));
        assert_eq!(
            output(&plan, AT).bytes(&disc).unwrap().as_ref(),
            two_members()
        );
        assert_eq!(
            output(&plan, PLAIN).bytes(&disc).unwrap().as_ref(),
            b"plain"
        );
    }

    /// Each change is sourced by what happened to it: an edited file is its
    /// own bytes, anything touched inside an archive rebuilds the whole
    /// archive, and a file or archive the disc never had is changed however
    /// its bytes look. The preamble sits untouched through all of it, still
    /// copied. What the rebuilt archives hold is the repack tests' business;
    /// only the decision is at stake here.
    #[test]
    fn a_change_is_sourced_by_what_happened_to_it() {
        let scratch = Scratch::new("planchanges");
        let (project, disc, vanilla) = on_disc(&scratch);
        write(&project.join(PLAIN), b"edited").unwrap();
        write(&project.join(AT).join("a.bin"), b"edited").unwrap();
        write(&project.join("files").join("new.bin"), b"new").unwrap();
        write(&project.join("files/new.arc").join("in.bin"), b"inner").unwrap();

        let plan = plan(&project, &disc, &vanilla).unwrap();
        let source = |path| &output(&plan, path).source;
        assert!(matches!(source(PLAIN), Source::Project(_)));
        assert!(matches!(source(AT), Source::Built(_)));
        assert!(matches!(source("files/new.bin"), Source::Project(_)));
        assert!(matches!(source("files/new.arc"), Source::Built(_)));
        assert!(matches!(source("sys/main.dol"), Source::Disc { .. }));
    }

    /// A deletion stays a deletion. A file gone from the project never enters
    /// the output, and a member gone from an archive rebuilds it: every file
    /// the project still holds agrees with its vanilla hash, so the member
    /// counts are the only thing left to notice.
    #[test]
    fn a_deletion_stays_a_deletion() {
        let scratch = Scratch::new("plandeleted");
        let (project, disc, vanilla) = on_disc(&scratch);
        fs::remove_file(project.join(PLAIN)).unwrap();
        fs::remove_file(project.join(AT).join("b.bin")).unwrap();

        let plan = plan(&project, &disc, &vanilla).unwrap();
        assert!(plan.outputs.iter().all(|output| output.path != PLAIN));
        assert!(plan.items.iter().all(|item| item.path() != PLAIN));
        assert!(matches!(output(&plan, AT).source, Source::Built(_)));
    }

    /// A status is the same walk `plan` opens with, laid out per leaf rather
    /// than folded into a build decision: an edited file, an edited member,
    /// an addition and a deletion each show up as their own line, archive
    /// membership included, and nothing untouched shows up at all.
    #[test]
    fn changes_lists_every_leaf_that_differs() {
        let scratch = Scratch::new("changes");
        let (project, _disc, vanilla) = on_disc(&scratch);
        write(&project.join(PLAIN), b"edited").unwrap();
        write(&project.join(AT).join("a.bin"), b"edited").unwrap();
        write(&project.join("files").join("new.bin"), b"new").unwrap();
        fs::remove_file(project.join(AT).join("b.bin")).unwrap();

        let changes = changes(&project, &vanilla).unwrap();
        let kind = |path: &str| {
            changes
                .iter()
                .find(|change| change.path == path)
                .unwrap_or_else(|| panic!("{path} should be reported"))
                .kind
        };
        assert!(matches!(kind(PLAIN), ChangeKind::Modified));
        assert!(matches!(kind(&format!("{AT}/a.bin")), ChangeKind::Modified));
        assert!(matches!(kind("files/new.bin"), ChangeKind::Added));
        assert!(matches!(kind(&format!("{AT}/b.bin")), ChangeKind::Deleted));
        assert_eq!(changes.len(), 4);
        assert!(changes.windows(2).all(|pair| pair[0].path < pair[1].path));
    }

    /// The sidecar is not a member, but it decides what the archive rebuilds
    /// into, so editing it alone has to count as touching the archive.
    #[test]
    fn an_edited_sidecar_rebuilds_the_archive() {
        let scratch = Scratch::new("plansidecar");
        let (project, disc, vanilla) = on_disc(&scratch);

        let mut sidecar = read_sidecar(&project.join(AT)).unwrap();
        // The last member, so the memory runs stay contiguous.
        sidecar.members[1].preload = Preload::Aram;
        write_sidecar(&sidecar, &project.join(AT)).unwrap();

        let plan = plan(&project, &disc, &vanilla).unwrap();
        assert!(matches!(output(&plan, AT).source, Source::Built(_)));
    }
}
