//! Building a project into what a target asked for.
//!
//! Only `mod/overlay/` is read. An overlay file sits at the same project path
//! `base/` holds it under, so the two layers stack: a rebuild takes the
//! overlay's copy where there is one and the vanilla copy everywhere else.
//! Nothing else under `mod/` reaches a disc, so no target looks at it.
//!
//! An unchanged file is never rebuilt.
//!
//! A target writes into a staging directory beside its output, swapped in
//! only once the whole build succeeded. A build that fails leaves the last
//! good one where it was.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::progress::{Progress, Step};
use crate::project::metadata::{self, Source};
use crate::project::{self, Staging};
use crate::{Result, fs};

mod implode;
mod targets;
mod tree;

pub use implode::EncodeError;
pub use targets::Target;

use tree::Tree;

/// What a build produced.
#[derive(Debug)]
pub struct Built {
    /// What to point somebody at: the tree for a patch, the file for an image.
    pub path: PathBuf,
    /// The disc files that were written again, at their disc paths.
    pub rebuilt: Vec<String>,
    /// Overlay files identical to vanilla, left out of the build.
    pub unchanged: Vec<String>,
}

/// Everything a target is handed.
pub struct Job<'a> {
    /// The project root.
    pub project: &'a Path,
    /// Overlay over vanilla, which is where every byte comes from.
    pub tree: &'a Tree,
    /// What `base/` says about itself.
    pub base: &'a metadata::Base,
    /// The disc this project was unpacked from.
    pub source: &'a Source,
    /// The disc files the overlay changed, each to be rebuilt.
    pub changed: &'a BTreeSet<String>,
    /// Where each step reports how far it has got.
    pub progress: &'a Progress,
}

/// Reads the project, works out what changed, and hands it to `target`.
///
/// The target's own directory under `build/targets/` is replaced whole on
/// every build. An `output` given instead can be anywhere, so it must be
/// missing or empty rather than emptied.
pub fn run(
    project: &Path,
    target: Target,
    output: Option<&Path>,
    progress: &Progress,
) -> Result<Built> {
    let out = match output {
        Some(output) => {
            project::refuse_unowned(output, &[])?;
            output.to_path_buf()
        }
        None => project::target_output(project, target.name()),
    };

    let metadata::Store { source, hashes } = metadata::read_store(project)?;
    let base = metadata::read_base(&project::base(project))?;
    let (tree, unchanged) = Tree::open(project, hashes)?;
    let changed = tree.changed();

    let job = Job {
        project,
        tree: &tree,
        base: &base,
        source: &source,
        changed: &changed,
        progress,
    };

    let staging = Staging::begin(&out)?;
    let produced = match target {
        Target::Patch => targets::patch::write(&job, staging.dir())?,
        Target::Image => targets::image::write(&job, staging.dir())?,
        Target::Dusk => targets::dusk::write(&job, staging.dir())?,
    };
    staging.promote()?;

    Ok(Built {
        path: out.join(produced),
        rebuilt: changed.into_iter().collect(),
        unchanged,
    })
}

/// Assembles every disc file the overlay changed, writing each into `into` at
/// its own project path.
///
/// One disc file has nothing to do with the next, so they go in parallel, the
/// same way an unpack takes them apart.
fn rebuild(job: &Job, into: &Path) -> Result<()> {
    let rebuilding = job.progress.begin(Step::Rebuild, job.changed.len() as u64);
    job.changed
        .par_iter()
        .map(|path| {
            let wrapped = job.base.yaz0_compressed.contains(path);
            let bytes = implode::disc_file(job.tree, path, wrapped)?;
            fs::write(&into.join(path), &bytes)?;
            rebuilding.add(1);
            Ok(())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tpmt_disc::{Bi2, Boot, Metadata};
    use tpmt_jkernel_arc::editable::sidecar::{Member, Sidecar};
    use tpmt_jkernel_compress::is_yaz0;

    use super::*;
    use crate::Error;
    use crate::project::metadata::sha1_hex;
    use crate::test_support::Scratch;
    use crate::unpack::explode;

    /// [`super::run`] with nobody watching its progress.
    fn run(project: &Path, target: Target, output: Option<&Path>) -> Result<Built> {
        super::run(project, target, output, &Progress::default())
    }

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
                unknown_1c: 1,
                unknown_20: 1,
                pad_spec: 0,
            },
        }
    }

    /// A project holding one wrapped archive of two members and one loose
    /// file, hashed the way an unpack would leave it.
    ///
    /// ```text
    /// files/outer.arc/plain.bin     "plain"
    /// files/outer.arc/wrapped.bin   "member", Yaz0 inside the archive
    /// files/loose.bin               "loose"
    /// ```
    fn unpacked(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        let project = &scratch.0;
        let base = project::base(project);

        let sidecar = Sidecar::new(
            "outer".to_string(),
            vec![
                Member {
                    path: "plain.bin".to_string(),
                    preload: tpmt_jkernel_arc::Preload::Mram,
                    yaz0_compressed: false,
                    id: Some(0),
                },
                Member {
                    path: "wrapped.bin".to_string(),
                    preload: tpmt_jkernel_arc::Preload::Mram,
                    yaz0_compressed: true,
                    id: Some(1),
                },
            ],
        );

        let files = [
            ("files/outer.arc/plain.bin", b"plain".to_vec()),
            ("files/outer.arc/wrapped.bin", b"member".to_vec()),
            ("files/loose.bin", b"loose".to_vec()),
            (
                "files/outer.arc/.tpmt-arc.toml",
                sidecar.to_toml().unwrap().into_bytes(),
            ),
        ];

        let mut hashes = BTreeMap::new();
        for (path, data) in &files {
            fs::write(&base.join(path), data).unwrap();
            hashes.insert((*path).to_string(), sha1_hex(data));
        }

        metadata::write_base(
            &base,
            &metadata(),
            BTreeSet::from(["files/outer.arc".to_string()]),
        )
        .unwrap();

        // Nothing in these tests opens the disc; `write_store` only wants a
        // path it can canonicalize.
        let iso = project.join("source.iso");
        fs::write(&iso, b"").unwrap();
        metadata::write_store(project, &iso, "0", &hashes, &metadata::Formats::new()).unwrap();

        scratch
    }

    fn overlay(project: &Path, path: &str, data: &[u8]) {
        fs::write(&project::overlay(project).join(path), data).unwrap();
    }

    /// Everything the built disc file explodes back into, which is what the
    /// unpack side would make of it.
    fn exploded(built: &Path, path: &str) -> BTreeMap<String, Vec<u8>> {
        let data = fs::read(&built.join(path)).unwrap();
        assert!(is_yaz0(&data), "the disc held this one wrapped");

        let mut outputs = BTreeMap::new();
        explode::file(path, &data, &mut |at, bytes| {
            outputs.insert(at.to_string(), bytes.to_vec());
            Ok(())
        })
        .unwrap();
        outputs
    }

    /// One edited member means the whole archive is written again, with the
    /// edit in it and every other member as it was.
    #[test]
    fn an_edited_member_rebuilds_its_archive() {
        let scratch = unpacked("member");
        overlay(&scratch.0, "files/outer.arc/plain.bin", b"edited");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, ["files/outer.arc"]);
        assert_eq!(built.unchanged, Vec::<String>::new());

        let outputs = exploded(&built.path, "files/outer.arc");
        assert_eq!(outputs["files/outer.arc/plain.bin"], b"edited");
        assert_eq!(outputs["files/outer.arc/wrapped.bin"], b"member");
    }

    /// A member's wrapper is the archive's to record, so a rebuild puts it
    /// back on the same member and leaves the other bare.
    #[test]
    fn members_keep_the_wrapper_they_arrived_with() {
        let scratch = unpacked("wrapper");
        overlay(&scratch.0, "files/outer.arc/plain.bin", b"edited");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        let outputs = exploded(&built.path, "files/outer.arc");
        let sidecar = Sidecar::from_toml(
            std::str::from_utf8(&outputs["files/outer.arc/.tpmt-arc.toml"]).unwrap(),
        )
        .unwrap();

        let wrapped: Vec<_> = sidecar
            .members
            .iter()
            .map(|member| (member.path.as_str(), member.yaz0_compressed))
            .collect();
        assert_eq!(wrapped, [("plain.bin", false), ("wrapped.bin", true)]);
        assert_eq!(sidecar.root, "outer");
    }

    /// A file the sidecar never mentioned is still a member, so somebody can
    /// add one by dropping it in the overlay.
    #[test]
    fn an_added_file_becomes_a_member() {
        let scratch = unpacked("added");
        overlay(&scratch.0, "files/outer.arc/extra.bin", b"extra");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        let outputs = exploded(&built.path, "files/outer.arc");
        assert_eq!(outputs["files/outer.arc/extra.bin"], b"extra");
        assert_eq!(outputs["files/outer.arc/plain.bin"], b"plain");
    }

    /// A loose file is its own disc file, and the archive beside it is left
    /// alone.
    #[test]
    fn a_loose_file_is_its_own_disc_file() {
        let scratch = unpacked("loose");
        overlay(&scratch.0, "files/loose.bin", b"replaced");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, ["files/loose.bin"]);
        assert_eq!(
            fs::read(&built.path.join("files/loose.bin")).unwrap(),
            b"replaced"
        );
        assert!(!built.path.join("files/outer.arc").exists());
    }

    /// An overlay file that matches vanilla is reported and left out of the
    /// build.
    #[test]
    fn an_edit_that_changes_nothing_is_reported() {
        let scratch = unpacked("noop");
        overlay(&scratch.0, "files/loose.bin", b"loose");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        assert_eq!(built.unchanged, ["files/loose.bin"]);
        assert_eq!(built.rebuilt, Vec::<String>::new());
    }

    /// `base/` is the disc. A build that read somebody's edit out of it would
    /// pack that edit as though it had shipped.
    #[test]
    fn an_edited_base_stops_the_build() {
        let scratch = unpacked("drift");
        fs::write(
            &project::base(&scratch.0).join("files/outer.arc/wrapped.bin"),
            b"tampered",
        )
        .unwrap();
        overlay(&scratch.0, "files/outer.arc/plain.bin", b"edited");

        let error = run(&scratch.0, Target::Patch, None).unwrap_err();
        assert!(
            matches!(&error, Error::BaseModified(path) if path == "files/outer.arc/wrapped.bin"),
            "{error}"
        );
    }

    /// A build that fails part way leaves the last good one in place, and
    /// nothing of its own beside it.
    #[test]
    fn a_failed_build_keeps_the_last_one() {
        let scratch = unpacked("keep");
        overlay(&scratch.0, "files/loose.bin", b"replaced");
        let built = run(&scratch.0, Target::Patch, None).unwrap();

        fs::write(
            &project::base(&scratch.0).join("files/outer.arc/wrapped.bin"),
            b"tampered",
        )
        .unwrap();
        overlay(&scratch.0, "files/outer.arc/plain.bin", b"edited");
        run(&scratch.0, Target::Patch, None).unwrap_err();

        assert_eq!(
            fs::read(&built.path.join("files/loose.bin")).unwrap(),
            b"replaced"
        );
        let targets = built.path.parent().unwrap();
        assert_eq!(std::fs::read_dir(targets).unwrap().count(), 1);
    }

    /// An empty overlay is not an error. There is simply nothing to write.
    #[test]
    fn an_empty_overlay_builds_nothing() {
        let scratch = unpacked("empty");

        let built = run(&scratch.0, Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, Vec::<String>::new());
        assert_eq!(std::fs::read_dir(&built.path).unwrap().count(), 0);
    }

    /// Every target owns its directory and clears it, so what is in there is
    /// what this build put there.
    #[test]
    fn a_target_clears_what_it_left_last_time() {
        let scratch = unpacked("clear");
        overlay(&scratch.0, "files/loose.bin", b"replaced");
        let built = run(&scratch.0, Target::Patch, None).unwrap();

        let stale = built.path.join("files/gone.bin");
        fs::write(&stale, b"from a build before").unwrap();
        run(&scratch.0, Target::Patch, None).unwrap();

        assert!(!stale.exists());
    }

    /// `-o` points wherever somebody says, and a build clears what it is
    /// given, so a directory holding anything else is refused rather than
    /// emptied.
    #[test]
    fn a_directory_of_somebody_elses_files_is_refused() {
        let scratch = unpacked("foreign");
        let out = scratch.0.join("elsewhere");
        fs::write(&out.join("notes.txt"), b"do not delete").unwrap();

        let error = run(&scratch.0, Target::Patch, Some(&out)).unwrap_err();
        assert!(
            matches!(&error, Error::ForeignDirectory(at) if *at == out),
            "{error}"
        );
        assert!(out.join("notes.txt").exists());
    }

    /// Nothing marks a directory as a build's own, so a second build to the
    /// same `-o` is refused like any other non-empty directory.
    #[test]
    fn an_output_directory_is_never_replaced() {
        let scratch = unpacked("reuse");
        let out = scratch.0.join("elsewhere");
        overlay(&scratch.0, "files/loose.bin", b"replaced");

        run(&scratch.0, Target::Patch, Some(&out)).unwrap();
        let error = run(&scratch.0, Target::Patch, Some(&out)).unwrap_err();
        assert!(
            matches!(&error, Error::ForeignDirectory(at) if *at == out),
            "{error}"
        );
        assert_eq!(fs::read(&out.join("files/loose.bin")).unwrap(), b"replaced");
    }

    #[test]
    fn dusk_says_it_is_not_here_yet() {
        let scratch = unpacked("dusk");

        let error = run(&scratch.0, Target::Dusk, None).unwrap_err();
        assert!(matches!(error, Error::Unsupported(Target::Dusk)), "{error}");
    }
}
