//! What a call found in its input besides its result, and how far it has got.
//!
//! A crate that checks something returns [`Diagnostic`]s next to its result,
//! each with a typed code and a typed place, so a test can match on them and a
//! frontend can point at the exact field. A check collects every problem it
//! can before it gives up, rather than stopping at the first.
//!
//! [`Report`] is the same thing with both turned to text and the file named,
//! for a list that mixes files and formats: a problems panel, or the CLI.
//!
//! A failure that stops the call before it can check anything still comes
//! back as its `Err`. An error diagnostic is one the call found and kept going
//! past, and whatever acts on the result (a save, a build) refuses while any
//! remain.
//!
//! What a call did, such as the files a build wrote, is its return value, not
//! a diagnostic.

use std::fmt;

mod progress;

pub use progress::{Counter, Progress, Snapshot, Step, Unit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The result can't be used until it's fixed.
    Error,
    /// Allowed, but not what the game expects.
    Warning,
    /// Worth knowing, and nothing to fix.
    Info,
}

impl Severity {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// One thing a check found: how much it matters, what it is, and where.
///
/// `code` is the checking crate's own enum. `at` is wherever that crate can
/// point: a byte range in a string, a message in a file, an entry in a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic<C, A> {
    pub severity: Severity,
    pub code: C,
    pub at: A,
}

impl<C, A> Diagnostic<C, A> {
    pub const fn error(code: C, at: A) -> Self {
        Self {
            severity: Severity::Error,
            code,
            at,
        }
    }

    pub const fn warning(code: C, at: A) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            at,
        }
    }

    pub const fn info(code: C, at: A) -> Self {
        Self {
            severity: Severity::Info,
            code,
            at,
        }
    }
}

impl<C: fmt::Display, A: fmt::Display> Diagnostic<C, A> {
    /// This diagnostic as a line in a list, found in the file at `path`.
    pub fn report(&self, path: &str) -> Report {
        Report {
            severity: self.severity,
            path: path.to_string(),
            at: Some(self.at.to_string()),
            text: self.code.to_string(),
        }
    }
}

/// A [`Diagnostic`] as text, with the project path of the file it's in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub severity: Severity,
    pub path: String,
    /// Where in the file, or `None` for the file as a whole.
    pub at: Option<String>,
    pub text: String,
}

impl Report {
    /// A report about the file at `path` as a whole.
    pub fn file(severity: Severity, path: &str, text: impl fmt::Display) -> Self {
        Self {
            severity,
            path: path.to_string(),
            at: None,
            text: text.to_string(),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}`", self.path)?;
        if let Some(at) = &self.at {
            write!(f, ", {at}")?;
        }
        write!(f, ": {}", self.text)
    }
}

/// How many reports there are of each severity, as a status bar shows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

impl Counts {
    #[must_use]
    pub fn of(reports: &[Report]) -> Self {
        let mut counts = Self::default();
        for report in reports {
            *match report.severity {
                Severity::Error => &mut counts.errors,
                Severity::Warning => &mut counts.warnings,
                Severity::Info => &mut counts.infos,
            } += 1;
        }
        counts
    }
}
