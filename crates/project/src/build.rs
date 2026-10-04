//! Building a project: works out what `mod/overlay/` changed, and hands it to
//! `tpmt-pipeline` for a target.
//!
//! A target writes into a staging directory beside its output, swapped in
//! only once the whole build succeeded. A build that fails leaves the last
//! good one where it was.

// TODO: only `mod/overlay/` reaches a build. `dusk` will also need
// `mod/res/` and `mod.json`, which `Job` has no way to carry yet.

use std::path::{Path, PathBuf};

use tpmt_pipeline::{Files, Job, Source, Target};
use tpmt_report::Progress;

use crate::io::{Staging, refuse_unowned};
use crate::store::{Digests, Store, digest};
use crate::{Comparison, Error, Layer, Layers, Project, Result};

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

impl Project {
    /// Re-encodes whatever `mod/overlay/` changed and hands it to `target`,
    /// which decides what to do with it: a tree of the changed disc files, a
    /// whole disc image, or a mod bundle.
    ///
    /// The target's own directory under `build/targets/` is replaced whole on
    /// every build. An `output` given instead can be anywhere, so it must be
    /// missing or empty rather than emptied.
    ///
    /// Reports [`tpmt_report::Step::Rebuild`] through `progress`, and for an
    /// image [`tpmt_report::Step::WriteImage`] as well.
    ///
    /// # Errors
    ///
    /// - [`Error::ForeignDirectory`] if `output` is not empty
    /// - [`Error::Io`] or [`Error::Parse`] if the project's own files cannot
    ///   be read
    /// - [`Error::BaseModified`] if `base/` no longer matches the disc it came
    ///   from
    /// - [`Error::Pipeline`] if a rebuilt file does not fit its format, or
    ///   whatever else the target needs, which for an image is the source disc
    pub fn build(
        &self,
        target: Target,
        output: Option<&Path>,
        progress: &Progress,
    ) -> Result<Built> {
        let out = match output {
            Some(output) => {
                refuse_unowned(output, &[])?;
                output.to_path_buf()
            }
            None => self.target_output(target.name()),
        };

        let Store { source, digests } = self.read_store()?;
        let base = self.read_base()?;
        let layers = self.layers();
        let Comparison { changes, identical } = layers.compare(&digests)?;
        let edits: Vec<String> = changes.into_iter().map(|change| change.path).collect();

        let name = self
            .root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(&base.metadata.boot.id);
        let job = Job {
            files: &Checked {
                layers: &layers,
                digests: &digests,
            },
            edits: &edits,
            metadata: &base.metadata,
            yaz0_compressed: &base.yaz0_compressed,
            source: Source {
                iso: &source.iso,
                id: &source.id,
                revision: source.revision,
            },
            name,
            progress,
        };

        let staging = Staging::begin(&out)?;
        let built = tpmt_pipeline::build(target, &job, staging.dir())?;
        staging.promote()?;

        Ok(Built {
            path: out.join(built.path),
            rebuilt: built.rebuilt,
            unchanged: identical,
        })
    }
}

/// The overlay over `base/`, with every `base/` copy held to its vanilla
/// digest on the way past.
///
/// A rebuild reads every unedited member straight out of `base/`, so a file
/// edited there in place would be packed as though the disc had shipped it.
/// A path the unpack never wrote fails the same way: either answer means
/// `base/` is no longer the disc it came from.
struct Checked<'a> {
    layers: &'a Layers,
    /// The vanilla digest of every file the unpack wrote.
    digests: &'a Digests,
}

impl Files<Error> for Checked<'_> {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        let (layer, data) = self.layers.read(path)?;
        let vanilla = self
            .digests
            .get(path)
            .is_some_and(|want| *want == digest(&data));
        if layer == Layer::Base && !vanilla {
            return Err(Error::BaseModified(path.to_string()));
        }
        Ok(data)
    }

    fn is_file(&self, path: &str) -> bool {
        self.layers.is_file(path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tempfile::TempDir;
    use tpmt_archive::editable::sidecar::{Member, Sidecar};
    use tpmt_disc::{Bi2, Boot, Metadata};

    use super::*;
    use crate::io::fs;
    use crate::store::Formats;
    use crate::{FileKind, base};

    /// [`Project::build`] with nobody watching its progress.
    fn run(project: &Path, target: Target, output: Option<&Path>) -> Result<Built> {
        Project::discover(project)?.build(target, output, &Progress::default())
    }

    /// A `GZ2E` revision 0 disc.
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
    fn unpacked() -> TempDir {
        let scratch = tempfile::tempdir().unwrap();
        let project = Project::claim(scratch.path()).unwrap();
        let base = project.base();

        let sidecar = Sidecar::new(
            "outer".to_string(),
            vec![
                Member {
                    path: "plain.bin".to_string(),
                    preload: tpmt_archive::Preload::Mram,
                    yaz0_compressed: false,
                    id: Some(0),
                },
                Member {
                    path: "wrapped.bin".to_string(),
                    preload: tpmt_archive::Preload::Mram,
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

        let mut digests = Digests::new();
        for (path, data) in &files {
            fs::write(&base.join(path), data).unwrap();
            digests.insert((*path).to_string(), digest(data));
        }

        base::write(
            &base,
            &metadata(),
            BTreeSet::from(["files/outer.arc".to_string()]),
        )
        .unwrap();

        // Nothing in these tests opens the disc; `write_store` only wants a
        // path it can canonicalize.
        let iso = scratch.path().join("source.iso");
        fs::write(&iso, b"").unwrap();
        project
            .write_store(&iso, &metadata().boot, &digests, &Formats::new())
            .unwrap();

        scratch
    }

    fn overlay(project: &Path, path: &str, data: &[u8]) {
        Project::discover(project)
            .unwrap()
            .write(path, data)
            .unwrap();
    }

    /// Everything the built disc file explodes back into, which is what the
    /// unpack side would make of it.
    fn exploded(built: &Path, path: &str) -> BTreeMap<String, Vec<u8>> {
        let data = fs::read(&built.join(path)).unwrap();
        FileKind::Yaz0
            .check(&data)
            .expect("the disc held this one wrapped");

        let mut outputs = BTreeMap::new();
        tpmt_pipeline::explode(
            path,
            &data,
            &mut |layer| -> tpmt_pipeline::Result<()> {
                if layer.leaf {
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
        overlay(scratch.path(), "files/outer.arc/plain.bin", b"edited");

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
        overlay(scratch.path(), "files/outer.arc/plain.bin", b"edited");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
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
        let scratch = unpacked();
        overlay(scratch.path(), "files/outer.arc/extra.bin", b"extra");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        let outputs = exploded(&built.path, "files/outer.arc");
        assert_eq!(outputs["files/outer.arc/extra.bin"], b"extra");
        assert_eq!(outputs["files/outer.arc/plain.bin"], b"plain");
    }

    /// A loose file is its own disc file, and the archive beside it is left
    /// alone.
    #[test]
    fn a_loose_file_is_its_own_disc_file() {
        let scratch = unpacked();
        overlay(scratch.path(), "files/loose.bin", b"replaced");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
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
        let scratch = unpacked();
        overlay(scratch.path(), "files/loose.bin", b"loose");

        let built = run(scratch.path(), Target::Patch, None).unwrap();
        assert_eq!(built.unchanged, ["files/loose.bin"]);
        assert_eq!(built.rebuilt, Vec::<String>::new());
    }

    /// `base/` is the disc. A build that read somebody's edit out of it would
    /// pack that edit as though it had shipped.
    #[test]
    fn an_edited_base_stops_the_build() {
        let scratch = unpacked();
        fs::write(
            &Project::discover(scratch.path())
                .unwrap()
                .base()
                .join("files/outer.arc/wrapped.bin"),
            b"tampered",
        )
        .unwrap();
        overlay(scratch.path(), "files/outer.arc/plain.bin", b"edited");

        let error = run(scratch.path(), Target::Patch, None).unwrap_err();
        assert!(
            matches!(&error, Error::BaseModified(path) if path == "files/outer.arc/wrapped.bin"),
            "{error}"
        );
    }

    /// A build that fails part way leaves the last good one in place, and
    /// nothing of its own beside it.
    #[test]
    fn a_failed_build_keeps_the_last_one() {
        let scratch = unpacked();
        overlay(scratch.path(), "files/loose.bin", b"replaced");
        let built = run(scratch.path(), Target::Patch, None).unwrap();

        fs::write(
            &Project::discover(scratch.path())
                .unwrap()
                .base()
                .join("files/outer.arc/wrapped.bin"),
            b"tampered",
        )
        .unwrap();
        overlay(scratch.path(), "files/outer.arc/plain.bin", b"edited");
        run(scratch.path(), Target::Patch, None).unwrap_err();

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
        overlay(scratch.path(), "files/loose.bin", b"replaced");
        let built = run(scratch.path(), Target::Patch, None).unwrap();

        let stale = built.path.join("files/gone.bin");
        fs::write(&stale, b"from a build before").unwrap();
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
        fs::write(&out.join("notes.txt"), b"do not delete").unwrap();

        let error = run(scratch.path(), Target::Patch, Some(&out)).unwrap_err();
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
        let scratch = unpacked();
        let out = scratch.path().join("elsewhere");
        overlay(scratch.path(), "files/loose.bin", b"replaced");

        run(scratch.path(), Target::Patch, Some(&out)).unwrap();
        let error = run(scratch.path(), Target::Patch, Some(&out)).unwrap_err();
        assert!(
            matches!(&error, Error::ForeignDirectory(at) if *at == out),
            "{error}"
        );
        assert_eq!(fs::read(&out.join("files/loose.bin")).unwrap(), b"replaced");
    }
}
