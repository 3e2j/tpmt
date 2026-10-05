//! Building a project: works out which files in `mod/changes/` differ from
//! vanilla, and hands them to packing for a target.

// TODO: only `mod/changes/` reaches a build. Every target will also need
// `mod/textures/`, and `dusk` needs `mod/res/` and `mod.json`, none of which
// `Job` has a way to carry yet.

use std::path::{Path, PathBuf};

use tpmt_editing::Version;
use tpmt_packing::{Files, Job, Metadata, Source, Target};
use tpmt_project::{Comparison, Digests, Overlay, Project, Store, Stored, Vanilla};
use tpmt_report::Progress;

use crate::{Error, Result};

/// What a build produced.
#[derive(Debug)]
pub struct Built {
    /// What to point somebody at: the tree for a patch, the file for an image.
    pub path: PathBuf,
    /// The disc files that were written again, at their disc paths.
    pub rebuilt: Vec<String>,
    /// Files in `changes/` identical to vanilla, left out of the build.
    pub unchanged: Vec<String>,
}

/// Re-encodes whatever `mod/changes/` changed and hands it to `target`, which
/// decides what to do with it: a tree of the changed disc files, a whole disc
/// image, or a mod bundle.
///
/// Writes into the target's own directory under `build/targets/`, or
/// `output` if given (see [`Project::stage_output`]).
///
/// Reports [`tpmt_report::Step::Rebuild`] through `progress`, and for an
/// image [`tpmt_report::Step::WriteImage`] as well.
///
/// # Errors
///
/// - [`tpmt_project::Error::ForeignDirectory`] if `output` is not empty
/// - [`tpmt_project::Error::Io`] or [`tpmt_project::Error::Parse`] if the
///   project's own files cannot be read
/// - [`tpmt_project::Error::VanillaModified`] if `vanilla/` no longer matches the
///   disc it came from
/// - [`Error::UnknownVersion`] or [`Error::File`] if a patch in `changes/`
///   can't be put back over its vanilla file
/// - [`tpmt_packing::Error`] if a rebuilt file does not fit its format, or
///   whatever else the target needs, which for an image is the source disc
pub fn build(
    project: &Project,
    target: Target,
    output: Option<&Path>,
    progress: &Progress,
) -> Result<Built> {
    let staging = project.stage_output(target.name(), output)?;

    let Store { source, digests } = project.read_store()?;
    let Vanilla { disc, compressed } = project.read_vanilla::<Metadata>()?;
    let overlay = project.overlay();
    let Comparison { changes, identical } = overlay.compare(&digests)?;
    let edits: Vec<String> = changes.into_iter().map(|change| change.path).collect();

    let name = project
        .root()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(&disc.boot.id);
    let job = Job {
        files: &Packed {
            overlay: &overlay,
            digests: &digests,
            version: Version::from_disc(&disc.boot.id, disc.boot.revision),
        },
        edits: &edits,
        metadata: &disc,
        compressed: &compressed,
        source: Source {
            iso: &source.iso,
            id: &source.id,
            revision: source.revision,
        },
        name,
        progress,
    };

    let built = tpmt_packing::build(target, &job, staging.dir())?;
    let path = staging.target().join(built.path);
    staging.promote()?;

    Ok(Built {
        path,
        rebuilt: built.rebuilt,
        unchanged: identical,
    })
}

/// Each project file as a build packs it: patches applied, and `vanilla/`
/// copies checked against their digests.
struct Packed<'a> {
    overlay: &'a Overlay,
    digests: &'a Digests,
    /// `None` when the disc isn't a known release, which only a patch needs.
    version: Option<Version>,
}

impl Files<Error> for Packed<'_> {
    fn read(&self, path: &str) -> Result<Box<[u8]>> {
        match self.overlay.read_checked(path, self.digests)? {
            Stored::Added(data) | Stored::Replaced(data) | Stored::Vanilla(data) => Ok(data),
            Stored::Patched { vanilla, edits } => {
                let version = self.version.ok_or(Error::UnknownVersion)?;
                tpmt_editing::apply(&vanilla, &edits, version)
                    .map(Vec::into_boxed_slice)
                    .map_err(Error::file(path))
            }
        }
    }

    fn is_file(&self, path: &str) -> bool {
        self.overlay.is_file(path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Read;

    use tempfile::TempDir;
    use tpmt_archive::editable::sidecar::{Member, Sidecar};
    use tpmt_binary::{Compression, FileKind};

    use super::*;
    use crate::fixture;

    /// [`build`] with nobody watching its progress.
    fn run(project: &Path, target: Target, output: Option<&Path>) -> Result<Built> {
        build(
            &Project::discover(project)?,
            target,
            output,
            &Progress::default(),
        )
    }

    /// Test files go straight to disk, outside any project.
    mod fs {
        use super::*;

        pub fn write(path: &Path, data: &[u8]) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }

        pub fn read(path: &Path) -> Vec<u8> {
            let mut data = Vec::new();
            std::fs::File::open(path)
                .unwrap()
                .read_to_end(&mut data)
                .unwrap();
            data
        }
    }

    /// A project holding one wrapped archive of two members and one loose
    /// file, hashed the way an unpack would leave it.
    ///
    /// ```text
    /// files/outer.arc/plain.bin     "plain"
    /// files/outer.arc/wrapped.bin   "member", Yay0 inside the archive
    /// files/loose.bin               "loose"
    /// ```
    fn unpacked() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        let project = Project::claim(scratch.path()).unwrap();
        let vanilla = project.new_vanilla().unwrap();

        let sidecar = Sidecar::new(
            "outer".to_string(),
            vec![
                Member {
                    path: "plain.bin".to_string(),
                    preload: tpmt_archive::Preload::Mram,
                    compression: None,
                    id: Some(0),
                },
                Member {
                    path: "wrapped.bin".to_string(),
                    preload: tpmt_archive::Preload::Mram,
                    compression: Some(Compression::Yay0),
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

        let written = files
            .iter()
            .map(|(path, data)| vanilla.write(path, None, data).unwrap())
            .collect();
        vanilla
            .finish(
                &fixture::metadata(),
                &BTreeMap::from([("files/outer.arc".to_string(), Compression::Yaz0)]),
            )
            .unwrap();

        fixture::finish(&project, scratch.path(), written);

        scratch
    }

    fn change(project: &Path, path: &str, data: &[u8]) {
        Project::discover(project)
            .unwrap()
            .write(path, data)
            .unwrap();
    }

    /// Everything the built disc file explodes back into, which is what the
    /// unpack side would make of it.
    fn exploded(built: &Path, path: &str) -> BTreeMap<String, Vec<u8>> {
        let data = fs::read(&built.join(path));
        FileKind::Yaz0
            .check(&data)
            .expect("the disc held this one wrapped");

        let mut outputs = BTreeMap::new();
        tpmt_packing::explode(
            path,
            &data,
            &mut |layer| -> tpmt_packing::Result<()> {
                if layer.stored {
                    outputs.insert(layer.path.to_string(), layer.bytes.to_vec());
                }
                Ok(())
            },
            &mut Vec::new(),
        )
        .unwrap();
        outputs
    }

    /// One edited member means the whole archive is written again, with the
    /// edit in it and every other member as it was.
    #[test]
    fn an_edited_member_rebuilds_its_archive() {
        let scratch = unpacked();
        change(scratch.path(), "files/outer.arc/plain.bin", b"edited");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
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
        let scratch = unpacked();
        change(scratch.path(), "files/outer.arc/plain.bin", b"edited");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        let outputs = exploded(&built.path, "files/outer.arc");
        let sidecar = Sidecar::from_toml(
            std::str::from_utf8(&outputs["files/outer.arc/.tpmt-arc.toml"]).unwrap(),
        )
        .unwrap();

        let wrapped: Vec<_> = sidecar
            .members
            .iter()
            .map(|member| (member.path.as_str(), member.compression))
            .collect();
        assert_eq!(
            wrapped,
            [
                ("plain.bin", None),
                ("wrapped.bin", Some(Compression::Yay0))
            ]
        );
        assert_eq!(sidecar.root, "outer");
    }

    /// A file the sidecar never mentioned is still a member, so somebody can
    /// add one by dropping it in `changes/`.
    #[test]
    fn an_added_file_becomes_a_member() {
        let scratch = unpacked();
        change(scratch.path(), "files/outer.arc/extra.bin", b"extra");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        let outputs = exploded(&built.path, "files/outer.arc");
        assert_eq!(outputs["files/outer.arc/extra.bin"], b"extra");
        assert_eq!(outputs["files/outer.arc/plain.bin"], b"plain");
    }

    /// A patch in `changes/` is put over the vanilla file it names, and the
    /// result is what the build writes.
    #[test]
    fn a_patch_builds_into_its_file() {
        let path = "files/message.bmg";
        let (scratch, project) = fixture::project(&[(path, &fixture::message_file(b"Hello"))]);
        project
            .overlay()
            .write_patch(path, b"[message.\"@0\"]\ntext = \"Goodbye\"\n")
            .unwrap();

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, [path]);
        assert_eq!(
            fs::read(&built.path.join(path)),
            fixture::message_file(b"Goodbye")
        );
    }

    /// A loose file is its own disc file, and the archive beside it is left
    /// alone.
    #[test]
    fn a_loose_file_is_its_own_disc_file() {
        let scratch = unpacked();
        change(scratch.path(), "files/loose.bin", b"replaced");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, ["files/loose.bin"]);
        assert_eq!(fs::read(&built.path.join("files/loose.bin")), b"replaced");
        assert!(!built.path.join("files/outer.arc").exists());
    }

    /// A file in `changes/` that matches vanilla is reported and left out of the
    /// build.
    #[test]
    fn an_edit_that_changes_nothing_is_reported() {
        let scratch = unpacked();
        change(scratch.path(), "files/loose.bin", b"loose");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        assert_eq!(built.unchanged, ["files/loose.bin"]);
        assert_eq!(built.rebuilt, Vec::<String>::new());
    }

    /// `vanilla/` is the disc. A build that read somebody's edit out of it would
    /// pack that edit as though it had shipped.
    #[test]
    fn an_edited_base_stops_the_build() {
        let scratch = unpacked();
        fs::write(
            &Project::discover(scratch.path())
                .unwrap()
                .vanilla()
                .join("files/outer.arc/wrapped.bin"),
            b"tampered",
        );
        change(scratch.path(), "files/outer.arc/plain.bin", b"edited");

        let error = run(scratch.path(), Target::Patch, None).unwrap_err();
        assert!(
            matches!(&error, Error::Project(tpmt_project::Error::VanillaModified(path)) if path == "files/outer.arc/wrapped.bin"),
            "{error}"
        );
    }

    /// A build that fails part way leaves the last good one in place, and
    /// nothing of its own beside it.
    #[test]
    fn a_failed_build_keeps_the_last_one() {
        let scratch = unpacked();
        change(scratch.path(), "files/loose.bin", b"replaced");
        let built = run(scratch.path(), Target::Patch, None).unwrap();

        fs::write(
            &Project::discover(scratch.path())
                .unwrap()
                .vanilla()
                .join("files/outer.arc/wrapped.bin"),
            b"tampered",
        );
        change(scratch.path(), "files/outer.arc/plain.bin", b"edited");
        run(scratch.path(), Target::Patch, None).unwrap_err();

        assert_eq!(fs::read(&built.path.join("files/loose.bin")), b"replaced");
        let targets = built.path.parent().unwrap();
        assert_eq!(std::fs::read_dir(targets).unwrap().count(), 1);
    }

    /// An empty `changes/` is not an error. There is simply nothing to write.
    #[test]
    fn empty_changes_build_nothing() {
        let scratch = unpacked();

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        assert_eq!(built.rebuilt, Vec::<String>::new());
        assert_eq!(std::fs::read_dir(&built.path).unwrap().count(), 0);
    }

    /// Every target owns its directory and clears it, so what is in there is
    /// what this build put there.
    #[test]
    fn a_target_clears_what_it_left_last_time() {
        let scratch = unpacked();
        change(scratch.path(), "files/loose.bin", b"replaced");
        let built = run(scratch.path(), Target::Patch, None).unwrap();

        let stale = built.path.join("files/gone.bin");
        fs::write(&stale, b"from a build before");
        run(scratch.path(), Target::Patch, None).unwrap();

        assert!(!stale.exists());
    }

    /// `-o` points wherever somebody says, and a build clears what it is
    /// given, so a directory holding anything else is refused rather than
    /// emptied.
    #[test]
    fn a_directory_of_somebody_elses_files_is_refused() {
        let scratch = unpacked();
        let out = scratch.path().join("elsewhere");
        fs::write(&out.join("notes.txt"), b"do not delete");

        let error = run(scratch.path(), Target::Patch, Some(&out)).unwrap_err();
        assert!(
            matches!(&error, Error::Project(tpmt_project::Error::ForeignDirectory(at)) if *at == out),
            "{error}"
        );
        assert!(out.join("notes.txt").exists());
    }

    /// Nothing marks a directory as a build's own, so a second build to the
    /// same `-o` is refused like any other non-empty directory.
    #[test]
    fn an_output_directory_is_never_replaced() {
        let scratch = unpacked();
        let out = scratch.path().join("elsewhere");
        change(scratch.path(), "files/loose.bin", b"replaced");

        run(scratch.path(), Target::Patch, Some(&out)).unwrap();
        let error = run(scratch.path(), Target::Patch, Some(&out)).unwrap_err();
        assert!(
            matches!(&error, Error::Project(tpmt_project::Error::ForeignDirectory(at)) if *at == out),
            "{error}"
        );
        assert_eq!(fs::read(&out.join("files/loose.bin")), b"replaced");
    }
}
