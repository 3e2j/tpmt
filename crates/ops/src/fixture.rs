//! Unpacked projects for tests, built by hand.

use std::collections::BTreeMap;

use tempfile::TempDir;
use tpmt_binary::{FileKind, Format};
use tpmt_disc::{Bi2, Boot, Metadata};
use tpmt_message::{Bmg, Encoding, Message, MessageId, TextSegment};
use tpmt_project::{Project, Record, Unpacking, Written};
use tpmt_tables::message::record;

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

/// A message file of one message saying `text`.
pub fn message_file(text: &[u8]) -> Vec<u8> {
    Bmg {
        encoding: Encoding::ShiftJis,
        record_len: record::LAYOUT.len,
        mid1: None,
        messages: vec![Message {
            public_id: 0,
            id: MessageId(0),
            attributes: Box::new([0; 16]),
            text: vec![TextSegment::Text(text.into())],
        }],
        flow: None,
        strings: None,
    }
    .encode()
    .unwrap()
}

/// A finished unpack of `metadata()` whose `vanilla/` holds `files`.
pub fn project(files: &[(&str, &[u8])]) -> (TempDir, Project) {
    let scratch = tempfile::tempdir().unwrap();
    let unpacking = Project::unpack(scratch.path()).unwrap();
    let written = files
        .iter()
        .map(|(path, data)| {
            unpacking
                .write(path, FileKind::identify(data), data)
                .unwrap()
        })
        .collect();
    let project = finish(unpacking, &scratch, written, &BTreeMap::new());
    (scratch, project)
}

/// Finishes an unpack of `metadata()` into `scratch`.
pub fn finish(
    unpacking: Unpacking,
    scratch: &TempDir,
    written: Vec<Written>,
    compressed: &BTreeMap<String, tpmt_binary::Compression>,
) -> Project {
    // Nothing in these tests opens the disc; the store only wants a path it
    // can canonicalize.
    let iso = scratch.path().join("source.iso");
    std::fs::write(&iso, b"").unwrap();
    let disc_metadata = metadata();
    let record = Record {
        disc: &iso,
        id: &disc_metadata.boot.id,
        revision: disc_metadata.boot.revision,
        disc_metadata: &disc_metadata,
        compressed,
        directories: &[],
    };
    unpacking.finish(&record, written).unwrap()
}
