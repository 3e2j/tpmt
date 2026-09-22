//! A scratch directory for tests, gone again when the test that made it ends.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Tagged with a counter as well as a name, since two tests picking the same
/// name would otherwise race on one path.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let unique = CALLS.fetch_add(1, Ordering::Relaxed);
        let at =
            std::env::temp_dir().join(format!("tpmt-test-{name}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        Self(at)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
