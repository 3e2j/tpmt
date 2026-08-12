//! Restoring project files back to what the source disc had.
//!
//! A plain file is disc bytes, byte for byte. A file inside an archive has
//! nothing but its vanilla hash to go on, so getting its bytes back means
//! unpacking the whole archive fresh off the disc into a scratch directory
//! and taking only the file (or files) revert actually asked for. That
//! scratch unpack walks the exact same route `unpack` did, so a reverted
//! member comes back exactly as it would have on day one: wrapper, nesting
//! and all.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sidecars::arc::{self, Manifest};
use crate::store::Store;
use crate::{Error, plan};

/// What reverting `path` would do, worked out before anything is written.
pub struct RevertPlan {
    /// The path revert was asked for, exactly as given.
    pub path: String,
    /// Every vanilla leaf that will be written back: `path` itself when it
    /// names one exactly, or everything under it when it names a directory.
    pub restore: Vec<String>,
    /// Leaves under `path` the disc never had, so there is nothing to put
    /// back. Left alone, only reported.
    pub skip: Vec<String>,
    /// A lone archive member also has its own entry in the archive's
    /// sidecar, restorable alongside it without touching any other member.
    /// `None` for anything else, `path` naming the sidecar included.
    pub arc_sidecar_entry: Option<SidecarEntry>,
}

/// An archive member's own entry in its sidecar, cascade-restorable
/// alongside the member itself.
pub struct SidecarEntry {
    /// The sidecar's own project path, for prompting.
    pub path: String,
}

/// Works out what reverting `path` would do, without touching anything.
pub(crate) fn plan(project: &Path, path: &str) -> Result<RevertPlan, Error> {
    let vanilla = Store::new(project).hashes()?;

    if vanilla.contains_key(path) {
        return Ok(RevertPlan {
            path: path.to_string(),
            restore: vec![path.to_string()],
            skip: Vec::new(),
            arc_sidecar_entry: sidecar_entry(project, &vanilla, path),
        });
    }

    let prefix = format!("{path}/");
    let mut restore: Vec<String> = vanilla
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect();
    if restore.is_empty() {
        return Err(Error::NotTracked(path.to_string()));
    }
    restore.sort();

    let mut skip = Vec::new();
    if project.join(path).is_dir() {
        let mut leaves = Vec::new();
        plan::gather(project, path, &mut leaves)?;
        skip.extend(
            leaves
                .into_iter()
                .filter(|leaf| !vanilla.contains_key(leaf)),
        );
        skip.sort();
    }

    Ok(RevertPlan {
        path: path.to_string(),
        restore,
        skip,
        arc_sidecar_entry: None,
    })
}

/// The archive `path` would cascade its revert into: the closest `.arc`
/// ancestor directory, not counting `path`'s own final component even when
/// that happens to end in `.arc` itself, since a foreign, non-RARC archive
/// kept whole is a leaf, not a container.
fn nearest_archive(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let mut nearest = None;
    let mut at = String::new();
    for part in &parts[..parts.len() - 1] {
        if !at.is_empty() {
            at.push('/');
        }
        at.push_str(part);
        if crate::is_archive(part) {
            nearest = Some(at.clone());
        }
    }
    nearest
}

/// Whether reverting `path` can also cascade into its archive's sidecar.
///
/// Only archive members get this: the archive format is the only one with a
/// sidecar of its own today. A future format crate that grows one (see
/// `sidecars/mod.rs`) would want the same treatment, but there is nothing
/// else to cascade into yet, so nothing more general is built here.
///
/// TODO: an isolated file's own sidecar, not nested under an archive, is not
/// covered at all: `nearest_archive` only recognizes an `.arc` ancestor, so
/// a future format with a sidecar of its own but no unpacked directory
/// (a bare file, sidecar sitting alongside it) would need its own lookup
/// path here, or a generalization of this one.
fn sidecar_entry(
    project: &Path,
    vanilla: &HashMap<String, String>,
    path: &str,
) -> Option<SidecarEntry> {
    let archive = nearest_archive(path)?;
    let sidecar = format!("{archive}/{}", arc::SIDECAR);
    // Reverting the sidecar itself is not a cascade into it, and one the
    // disc never had, or one already gone from the project, has nothing to
    // splice a member's entry into.
    if path == sidecar || !vanilla.contains_key(&sidecar) || !project.join(&sidecar).exists() {
        return None;
    }
    Some(SidecarEntry { path: sidecar })
}

/// Carries out a plan `plan` already worked out, restoring every leaf it
/// names and, if asked, cascading into the sidecar entry it found.
pub(crate) fn apply(
    project: &Path,
    revert: &RevertPlan,
    restore_sidecar_entry: bool,
) -> Result<(), Error> {
    let (_, disc, _) = crate::open(project)?;
    let entries = disc.entries()?;
    let on_disc = crate::on_disc(&entries);

    // Grouped by the outermost archive each target belongs to (or itself,
    // for a plain file), so an archive several targets share is only
    // unpacked off the disc once.
    let mut by_owner: HashMap<&str, Vec<&str>> = HashMap::new();
    for target in &revert.restore {
        by_owner
            .entry(plan::owner(target))
            .or_default()
            .push(target);
    }

    for (owner, targets) in by_owner {
        let (offset, size) = *on_disc
            .get(owner)
            .ok_or_else(|| Error::NotOnDisc(owner.to_string()))?;
        let bytes = disc.read(offset, size)?;

        // The owner is its own only target exactly when nothing was
        // unpacked out of it: a plain file, or a foreign `.arc` this
        // toolkit never opened. Either way the disc bytes already are the
        // target's bytes.
        if targets.len() == 1 && targets[0] == owner {
            crate::fs::write(&project.join(owner), &bytes)?;
            continue;
        }

        let scratch = Scratch::new()?;
        crate::unpack_archive(&bytes, &scratch.path().join(owner), owner, &mut Vec::new())?;

        // Everything that can fail (reading a member's fresh bytes,
        // reading and patching the two manifests for a cascade) happens
        // before anything is written, so a project this touches ends up
        // either fully reverted or not touched at all, never caught
        // between by an error partway through.
        let mut data = Vec::with_capacity(targets.len());
        for target in &targets {
            data.push((*target, crate::fs::read(&scratch.path().join(target))?));
        }
        let cascade = match restore_sidecar_entry {
            true => sidecar_update(
                project,
                scratch.path(),
                &revert.arc_sidecar_entry,
                &revert.path,
            )?,
            false => None,
        };

        for (target, bytes) in data {
            crate::fs::write(&project.join(target), &bytes)?;
        }
        if let Some((archive, manifest)) = cascade {
            manifest.write(&project.join(archive))?;
        }
    }

    Ok(())
}

/// Works out the sidecar update a cascade would write, restoring one
/// member's own entry and leaving every other member's entry and the
/// sidecar's own order untouched. Gives back nothing written yet, so a
/// caller can hold it until every other part of the revert is ready too.
fn sidecar_update(
    project: &Path,
    scratch: &Path,
    entry: &Option<SidecarEntry>,
    member: &str,
) -> Result<Option<(String, Manifest)>, Error> {
    let Some(entry) = entry else { return Ok(None) };
    let archive = entry
        .path
        .strip_suffix(&format!("/{}", arc::SIDECAR))
        .expect("a sidecar entry's path always ends in the sidecar's own name");
    let relative = member
        .strip_prefix(&format!("{archive}/"))
        .expect("a member with a sidecar entry always sits under that entry's archive");

    let vanilla = Manifest::read(&scratch.join(archive))?;
    let Some(original) = vanilla
        .members
        .iter()
        .find(|member| member.path == relative)
    else {
        return Ok(None);
    };

    let mut current = Manifest::read(&project.join(archive))?;
    let Some(member) = current
        .members
        .iter_mut()
        .find(|member| member.path == relative)
    else {
        return Ok(None);
    };
    *member = original.clone();

    Ok(Some((archive.to_string(), current)))
}

/// A scratch directory to unpack a vanilla archive into, gone again once
/// revert is done reading out of it.
struct Scratch(PathBuf);

impl Scratch {
    /// The process id alone is not enough: several reverts in flight at
    /// once in the same process (several archives in one bulk revert, or
    /// several tests in one binary) would all land on the same path, so a
    /// counter tags each one its own.
    fn new() -> Result<Self, Error> {
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let unique = CALLS.fetch_add(1, Ordering::Relaxed);

        let at = std::env::temp_dir().join(format!("tpmt-revert-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&at);
        crate::fs::create_dir(&at)?;
        Ok(Self(at))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use tpmt_arc::Preload;
    use tpmt_disc::{Bi2, Boot, Entry as DiscEntry, Item, Layout, Metadata};

    use super::*;
    use crate::fs::write;
    use crate::sidecars::arc::Preload as SidecarPreload;

    /// A directory to work in, gone again when the test that made it ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let at = std::env::temp_dir()
                .join(format!("tpmt-revert-test-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&at);
            fs::create_dir_all(&at).unwrap();
            Self(at)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PLAIN: &str = "files/plain.bin";
    const AT: &str = "files/thing.arc";

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

    fn archive() -> Vec<u8> {
        tpmt_arc::pack(&tpmt_arc::Archive {
            root: "archive".to_string(),
            files: vec![
                tpmt_arc::File {
                    path: "a.bin".to_string(),
                    data: b"first",
                    id: None,
                    preload: Preload::Mram,
                },
                tpmt_arc::File {
                    path: "b.bin".to_string(),
                    data: b"second",
                    id: None,
                    preload: Preload::Mram,
                },
            ],
            ..Default::default()
        })
        .unwrap()
    }

    /// A disc holding one plain file and a two-member archive, unpacked into
    /// a fresh project: what every revert test starts from.
    fn project(scratch: &Scratch) -> PathBuf {
        let apploader = vec![0u8; 0x20];
        let dol = vec![0u8; 0x100];
        let archive = archive();
        let files: Vec<(&str, &[u8])> = vec![
            ("sys/apploader.img", &apploader),
            ("sys/main.dol", &dol),
            (PLAIN, b"plain"),
            (AT, &archive),
        ];

        let items: Vec<Item> = files
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
            let DiscEntry::File { path, .. } = entry else {
                continue;
            };
            let (_, data) = files.iter().find(|(at, _)| at == path).unwrap();
            image.file(data).unwrap();
        }
        image.finish().unwrap();

        let iso = scratch.0.join("source.iso");
        fs::write(&iso, out).unwrap();

        let project = scratch.0.join("project");
        crate::unpack(&iso, &project).unwrap();
        project
    }

    /// The plainest case: a top-level file, reverted straight from the disc,
    /// no archive and no sidecar in the way.
    #[test]
    fn reverts_a_plain_file() {
        let scratch = Scratch::new("plain");
        let project = project(&scratch);
        write(&project.join(PLAIN), b"edited").unwrap();

        let plan = plan(&project, PLAIN).unwrap();
        assert_eq!(plan.restore, [PLAIN]);
        assert!(plan.arc_sidecar_entry.is_none());

        apply(&project, &plan, false).unwrap();
        assert_eq!(crate::fs::read(&project.join(PLAIN)).unwrap(), b"plain");
    }

    /// A file the project no longer has is exactly as revertable as one
    /// somebody edited: nothing here should have to exist on disk first.
    #[test]
    fn reverts_a_deleted_file() {
        let scratch = Scratch::new("deleted");
        let project = project(&scratch);
        fs::remove_file(project.join(PLAIN)).unwrap();

        let plan = plan(&project, PLAIN).unwrap();
        apply(&project, &plan, false).unwrap();
        assert_eq!(crate::fs::read(&project.join(PLAIN)).unwrap(), b"plain");
    }

    /// An archive member comes back by unpacking the archive fresh off the
    /// disc, not by reading disc bytes straight: this is the path that
    /// exercises that scratch unpack.
    #[test]
    fn reverts_an_archive_member() {
        let scratch = Scratch::new("member");
        let project = project(&scratch);
        let member = format!("{AT}/a.bin");
        write(&project.join(&member), b"edited").unwrap();

        let plan = plan(&project, &member).unwrap();
        assert_eq!(plan.restore, [member.as_str()]);
        assert_eq!(
            plan.arc_sidecar_entry.as_ref().unwrap().path,
            format!("{AT}/.tpmt-arc.toml")
        );

        apply(&project, &plan, false).unwrap();
        assert_eq!(crate::fs::read(&project.join(&member)).unwrap(), b"first");
    }

    /// Cascading into the sidecar restores only the one member's own entry:
    /// a preload somebody set on a different member is left exactly as they
    /// left it.
    #[test]
    fn cascades_into_only_its_own_sidecar_entry() {
        let scratch = Scratch::new("cascade");
        let project = project(&scratch);
        let member = format!("{AT}/a.bin");
        write(&project.join(&member), b"edited").unwrap();

        let mut manifest = Manifest::read(&project.join(AT)).unwrap();
        manifest.members[0].preload = SidecarPreload::Aram;
        manifest.members[1].preload = SidecarPreload::Aram;
        manifest.write(&project.join(AT)).unwrap();

        let plan = plan(&project, &member).unwrap();
        apply(&project, &plan, true).unwrap();

        let after = Manifest::read(&project.join(AT)).unwrap();
        assert_eq!(after.members[0].preload, SidecarPreload::Mram);
        assert_eq!(after.members[1].preload, SidecarPreload::Aram);
    }

    /// Declining the cascade restores the member's bytes alone: the sidecar
    /// is somebody else's edit to leave alone unless they say otherwise.
    #[test]
    fn without_cascading_the_sidecar_is_left_alone() {
        let scratch = Scratch::new("nocascade");
        let project = project(&scratch);
        let member = format!("{AT}/a.bin");
        write(&project.join(&member), b"edited").unwrap();

        let mut manifest = Manifest::read(&project.join(AT)).unwrap();
        manifest.members[0].preload = SidecarPreload::Aram;
        manifest.write(&project.join(AT)).unwrap();

        let plan = plan(&project, &member).unwrap();
        apply(&project, &plan, false).unwrap();

        let after = Manifest::read(&project.join(AT)).unwrap();
        assert_eq!(after.members[0].preload, SidecarPreload::Aram);
    }

    /// A directory target reverts every vanilla leaf under it in one go,
    /// sidecar included, while an untracked file sitting alongside them is
    /// reported rather than touched.
    #[test]
    fn reverts_everything_under_a_directory() {
        let scratch = Scratch::new("directory");
        let project = project(&scratch);
        write(&project.join(AT).join("a.bin"), b"edited").unwrap();
        fs::remove_file(project.join(AT).join("b.bin")).unwrap();
        write(&project.join(AT).join("extra.bin"), b"untracked").unwrap();

        let plan = plan(&project, AT).unwrap();
        assert_eq!(
            plan.restore,
            [
                format!("{AT}/.tpmt-arc.toml"),
                format!("{AT}/a.bin"),
                format!("{AT}/b.bin"),
            ]
        );
        assert_eq!(plan.skip, [format!("{AT}/extra.bin")]);
        assert!(plan.arc_sidecar_entry.is_none());

        apply(&project, &plan, false).unwrap();
        assert_eq!(
            crate::fs::read(&project.join(AT).join("a.bin")).unwrap(),
            b"first"
        );
        assert_eq!(
            crate::fs::read(&project.join(AT).join("b.bin")).unwrap(),
            b"second"
        );
        assert_eq!(
            crate::fs::read(&project.join(AT).join("extra.bin")).unwrap(),
            b"untracked"
        );
    }

    /// A path the disc never had, directly or under it, has nothing to
    /// revert to.
    #[test]
    fn refuses_a_path_thats_not_tracked() {
        let scratch = Scratch::new("untracked");
        let project = project(&scratch);
        write(&project.join("files/new.bin"), b"new").unwrap();

        let error = match plan(&project, "files/new.bin") {
            Err(error) => error,
            Ok(_) => panic!("an untracked path should refuse to plan a revert"),
        };
        assert!(matches!(error, Error::NotTracked(path) if path == "files/new.bin"));
    }
}
