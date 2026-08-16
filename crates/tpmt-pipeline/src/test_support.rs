//! A scratch directory for tests to work in, gone again when the test that
//! made it ends.
//!
//! One process runs every test in this crate, so a name alone is not enough
//! to keep two scratch directories apart: two tests in different modules
//! picking the same name, run in parallel by cargo's own test threads,
//! would otherwise race to create and clear the same path. A counter tags
//! each one its own regardless of what name was asked for.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let unique = CALLS.fetch_add(1, Ordering::Relaxed);

        let at =
            std::env::temp_dir().join(format!("tpmt-test-{name}-{}-{unique}", std::process::id()));
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
