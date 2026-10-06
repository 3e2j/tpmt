//! Checking every file `mod/changes/` changes, without building anything.
//!
//! A patch that won't apply is an error on its file, and each format's own
//! checks add warnings. Every file is checked before anything is returned, so
//! one run names every problem. A build runs the same pass first, and stops
//! if it found any error.

use std::collections::HashMap;

use tpmt_editing::{Session, Source, Version};
use tpmt_packing::Metadata;
use tpmt_project::{Comparison, Digests, Overlay, Project, Store, Stored};
use tpmt_report::{Report, Severity};

use crate::{Error, Result, version};

/// Everything odd about the files in `mod/changes/`, sorted by path.
///
/// # Errors
///
/// Only for what stops the check itself. A problem in one file is a report.
///
/// - [`tpmt_project::Error`] if the project's own files can't be read, or
///   `vanilla/` no longer matches the disc it came from
/// - [`Error::UnknownVersion`] if a patch needs tables tpmt doesn't have
pub fn check(project: &Project) -> Result<Vec<Report>> {
    let Store { digests, .. } = project.read_store()?;
    let disc = project.read_disc::<Metadata>()?;
    let overlay = project.overlay();
    let comparison = overlay.compare(&digests)?;
    Ok(Checked::run(&overlay, &digests, version(&disc), &comparison)?.reports)
}

/// What [`check`] found, and what it made on the way that a build reuses.
pub struct Checked {
    pub reports: Vec<Report>,
    /// Each patched file with its patch put over it, by path, so a build
    /// doesn't apply it a second time.
    pub patched: HashMap<String, Box<[u8]>>,
}

impl Checked {
    pub fn run(
        overlay: &Overlay,
        digests: &Digests,
        version: Option<Version>,
        comparison: &Comparison,
    ) -> Result<Self> {
        let mut checked = Self {
            reports: Vec::new(),
            patched: HashMap::new(),
        };
        for path in &comparison.identical {
            checked.reports.push(Report::file(
                Severity::Warning,
                path,
                "it's identical to vanilla, so it changes nothing",
            ));
        }
        for change in &comparison.changes {
            checked.file(overlay, digests, version, &change.path)?;
        }
        Ok(checked)
    }

    fn file(
        &mut self,
        overlay: &Overlay,
        digests: &Digests,
        version: Option<Version>,
        path: &str,
    ) -> Result<()> {
        let stored = overlay.read_checked(path, digests)?;
        let (source, patched) = match &stored {
            Stored::Whole(bytes) => (Source::Whole(bytes), false),
            Stored::Vanilla { vanilla, patch } => (
                Source::Vanilla {
                    vanilla,
                    patch: patch.as_deref(),
                },
                patch.is_some(),
            ),
        };
        // A whole file only needs the tables for its warnings, but a patch
        // can't be put over vanilla without them.
        let Some(version) = version else {
            return if patched {
                Err(Error::UnknownVersion)
            } else {
                Ok(())
            };
        };

        let session = match Session::open(source, version) {
            Ok(session) => session,
            Err(error) if error.is_not_editable() && !patched => return Ok(()),
            Err(error) => {
                self.failed(path, &error);
                return Ok(());
            }
        };
        self.reports.extend(session.reports(path));
        if patched {
            match session.encode() {
                Ok(bytes) => {
                    self.patched.insert(path.to_string(), bytes.into());
                }
                Err(error) => self.failed(path, &error),
            }
        }
        Ok(())
    }

    /// Reports every problem behind `error`, or `error` itself when it is
    /// the only one.
    fn failed(&mut self, path: &str, error: &tpmt_editing::Error) {
        let reports = error.reports(path);
        if reports.is_empty() {
            self.reports
                .push(Report::file(Severity::Error, path, error));
        } else {
            self.reports.extend(reports);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    const PATH: &str = "files/message.bmg";

    fn patched(patch: &str) -> (tempfile::TempDir, Project) {
        let (scratch, project) = fixture::project(&[(PATH, &fixture::message_file(b"Hello"))]);
        project
            .overlay()
            .write_patch(PATH, patch.as_bytes())
            .unwrap();
        (scratch, project)
    }

    /// Every bad entry in a patch is its own report, named by the entry.
    #[test]
    fn every_bad_entry_is_reported() {
        let (_scratch, project) =
            patched("[message.\"@0\"]\ntext = \"{No tag}\"\n\n[message.\"@9\"]\ntext = \"x\"\n");
        let reports = check(&project).unwrap();
        let found: Vec<_> = reports
            .iter()
            .map(|report| (report.severity, report.at.as_deref(), report.text.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    Severity::Error,
                    Some("[message.\"@0\"]"),
                    "`No tag` is not a tag"
                ),
                (
                    Severity::Error,
                    Some("[message.\"@9\"]"),
                    "no vanilla message is `@9`"
                ),
            ]
        );
        assert!(reports.iter().all(|report| report.path == PATH));
    }

    /// A tag the tables don't know is allowed, and warned about on the
    /// message that has it.
    #[test]
    fn an_unknown_tag_is_a_warning() {
        let (_scratch, project) = patched("[message.\"@0\"]\ntext = \"{#7.3}\"\n");
        assert_eq!(
            check(&project).unwrap(),
            [Report {
                severity: Severity::Warning,
                path: PATH.to_string(),
                at: Some("message @0 text".to_string()),
                text: "no tag is group 7, code 3 in this version".to_string(),
            }]
        );
    }
}
