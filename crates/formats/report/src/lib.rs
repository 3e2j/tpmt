//! Whatever a call has to tell the user besides its result, and how far it
//! has got. A call sends both through a [`Progress`] as it goes, and the
//! caller decides what each report means for it: a log line, a message on
//! stderr, or a reason to exit.
//!
//! A failure that stops the call still comes back as its `Err`.
//! [`Level::Error`] is for one the call carried on past.

use std::fmt;

mod progress;

pub use progress::{Counter, Progress, Snapshot, Step, Unit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    /// Work that finished as asked.
    Ok,
    Warn,
    Error,
}

impl Level {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Ok => "ok",
            Self::Warn => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub level: Level,
    pub text: String,
}

impl Report {
    pub fn info(text: impl fmt::Display) -> Self {
        Self::new(Level::Info, text)
    }

    pub fn ok(text: impl fmt::Display) -> Self {
        Self::new(Level::Ok, text)
    }

    pub fn warn(text: impl fmt::Display) -> Self {
        Self::new(Level::Warn, text)
    }

    pub fn error(text: impl fmt::Display) -> Self {
        Self::new(Level::Error, text)
    }

    fn new(level: Level, text: impl fmt::Display) -> Self {
        Self {
            level,
            text: text.to_string(),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}
