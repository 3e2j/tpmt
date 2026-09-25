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

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// One stage of a command, in the order the commands reach them.
///
/// Numbered from one, so zero is free to mean no step has begun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    /// SHA-1 over the whole source disc before a build, counted in bytes.
    HashDisc = 1,
    /// Reading the disc once, hashing it and taking every file apart into
    /// `base/` on the way, counted in image bytes read.
    Unpack,
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
    pub const ALL: [Self; 5] = [
        Self::HashDisc,
        Self::Unpack,
        Self::Save,
        Self::Rebuild,
        Self::WriteImage,
    ];

    /// What the step is doing, as a short present-tense phrase.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::HashDisc => "hashing disc",
            Self::Unpack => "unpacking disc",
            Self::Save => "saving project",
            Self::Rebuild => "rebuilding files",
            Self::WriteImage => "writing image",
        }
    }

    #[must_use]
    pub const fn unit(self) -> Unit {
        match self {
            Self::HashDisc | Self::Unpack | Self::WriteImage => Unit::Bytes,
            Self::Rebuild => Unit::Files,
            Self::Save => Unit::None,
        }
    }
}

/// The step a command is on and its tally. Hand one to a pipeline call and
/// read it from another thread while the call runs.
#[derive(Default)]
pub struct Progress {
    step: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
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
        progress.begin(Step::HashDisc, 100).add(100);
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
}
