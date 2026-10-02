//! What tests share: a scratch directory, gone again when the test that made
//! it ends, and the disc metadata a project fixture records.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tpmt_disc::{Bi2, Boot, Metadata};

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

/// A `GZ2E` revision 0 disc.
pub fn metadata() -> Metadata {
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
