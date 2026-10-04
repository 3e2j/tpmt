//! How far a running command has got, for whoever is watching it.
//!
//! The pipeline only counts. A command's steps run one after another, so
//! [`Progress`] holds the current one and its tally. Workers add to it with
//! relaxed atomics, so a parallel loop pays one `fetch_add` per file. A
//! frontend reads [`Progress::current`] on its own clock and draws it however
//! it likes.
//!
//! Every counter is read on its own, so a reader can catch a new step with
//! the last one's numbers for a moment. Treat `done` past `total` as finished.
//!
//! Reports queue up until a frontend takes them with [`Progress::take_reports`],
//! in the order files finish.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use tpmt_report::Report;

/// One stage of a command, in the order the commands reach them.
///
/// Numbered from one, so zero is free to mean no step has begun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    /// Reading the disc once and taking every file apart into `base/` on the
    /// way, counted in file bytes read.
    Unpack = 1,
    /// Writing the project's own files and swapping `base/` in. No total.
    Save,
    /// Re-encoding each changed disc file, counted in files.
    Rebuild,
    /// Laying out a whole disc image, counted in file bytes.
    WriteImage,
}

/// What a step's `done` and `total` count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Files,
    /// The step has no measure of its own, only whether it is running.
    None,
}

impl Step {
    pub const ALL: [Self; 4] = [Self::Unpack, Self::Save, Self::Rebuild, Self::WriteImage];

    /// What the step is doing, as a short present-tense phrase.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unpack => "unpacking disc",
            Self::Save => "saving project",
            Self::Rebuild => "rebuilding files",
            Self::WriteImage => "writing image",
        }
    }

    #[must_use]
    pub const fn unit(self) -> Unit {
        match self {
            Self::Unpack | Self::WriteImage => Unit::Bytes,
            Self::Rebuild => Unit::Files,
            Self::Save => Unit::None,
        }
    }
}

/// The step a command is on, its tally, and the reports no one has taken
/// yet. Hand one to a pipeline call and read it from another thread while the
/// call runs.
#[derive(Default)]
pub struct Progress {
    step: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    reports: Mutex<Vec<Report>>,
}

/// The current step, as [`Progress::current`] saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub step: Step,
    pub done: u64,
    /// Zero for a step whose [`Unit`] is [`Unit::None`].
    pub total: u64,
}

impl Progress {
    /// Makes `step` the current one, with `total` to go. It stays current,
    /// finished or not, until the next step begins.
    pub(crate) fn begin(&self, step: Step, total: u64) -> Counter<'_> {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.step.store(step as u8, Ordering::Relaxed);
        Counter(&self.done)
    }

    /// The step the command is on, or `None` before the first one begins.
    pub fn current(&self) -> Option<Snapshot> {
        let raw = self.step.load(Ordering::Relaxed);
        let step = Step::ALL.into_iter().find(|&step| step as u8 == raw)?;
        Some(Snapshot {
            step,
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
        })
    }

    pub(crate) fn report(&self, reports: impl IntoIterator<Item = Report>) {
        // A panic inside `extend` still leaves a valid list, so poison is safe
        // to ignore.
        self.reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(reports);
    }

    /// Every report since the last call, oldest first. Take them once more
    /// after the call returns, since it may have added some after the last
    /// look.
    pub fn take_reports(&self) -> Vec<Report> {
        std::mem::take(&mut *self.reports.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Where the current step's workers add what they got through.
pub struct Counter<'a>(&'a AtomicU64);

impl Counter<'_> {
    pub fn add(&self, amount: u64) {
        self.0.fetch_add(amount, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_shows_before_a_step_begins() {
        assert_eq!(Progress::default().current(), None);
    }

    #[test]
    fn a_new_step_replaces_the_last() {
        let progress = Progress::default();
        progress.begin(Step::Rebuild, 100).add(100);
        progress.begin(Step::Unpack, 10).add(4);
        assert_eq!(
            progress.current(),
            Some(Snapshot {
                step: Step::Unpack,
                done: 4,
                total: 10
            })
        );
    }

    #[test]
    fn taking_reports_empties_them() {
        let progress = Progress::default();
        progress.report([Report::warn("a"), Report::warn("b")]);
        assert_eq!(
            progress.take_reports(),
            [Report::warn("a"), Report::warn("b")]
        );
        assert_eq!(progress.take_reports(), []);
    }
}
