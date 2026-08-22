use tpmt_bytes::{Reader, Writer};

use super::*;

/// The fixture almost every test opens or corrupts: a root holding `a.bin`
/// and a subdirectory `sub` holding `b.bin`. It comes out of [`pack`]
/// rather than being spelled by hand, and `packs_the_retail_layout` below
/// is what pins that output to the conventions byte by byte, so the two
/// together are not circular.
fn fixture() -> Vec<File<'static>> {
    vec![
        File {
            path: "a.bin".into(),
            data: b"AAAAA",
            id: None,
            preload: Preload::Mram,
        },
        File {
            path: "sub/b.bin".into(),
            data: b"BBB",
            id: None,
            preload: Preload::Mram,
        },
    ]
}

fn archive() -> Vec<u8> {
    pack(&Archive {
        root: "root".into(),
        files: fixture(),
        ..Default::default()
    })
    .unwrap()
}

// Where that fixture's sections land: two headers, two nodes from 0x40,
// seven entries from 0x60 ending 0xEC, the pool 0x20 aligned after them,
// and the file data after its padded 0x1A bytes.
const NODES: usize = 0x40;
const ENTRIES: usize = 0x60;
const STRINGS: usize = 0x100;
const FILE_DATA: usize = 0x120;

// Positions within the pool `.\0..\0root\0a.bin\0sub\0b.bin\0`.
const NAME_A: usize = 0x0A;

/// The whole fixture image checked field by field against the layout the
/// retail discs use. Everything else in this module trusts `pack`, and
/// this is what earns that: any drift in the conventions lands here.
#[test]
fn packs_the_retail_layout() {
    let data = archive();
    let r = Reader::new(&data);
    assert_eq!(data.len(), 0x160);

    assert_eq!(&data[..4], b"RARC");
    assert_eq!(r.u32_at(top_header::FILE_SIZE).unwrap(), 0x160);
    assert_eq!(r.u32_at(top_header::DATA_HEADER_PTR).unwrap(), 0x20);
    assert_eq!(r.u32_at(top_header::FILE_DATA_PTR).unwrap(), 0x100);
    assert_eq!(r.u32_at(top_header::TOTAL_DATA_SIZE).unwrap(), 0x40);
    assert_eq!(r.u32_at(top_header::MRAM_SIZE).unwrap(), 0x40);
    assert_eq!(r.u32_at(top_header::ARAM_SIZE).unwrap(), 0);
    // The unnamed tail of the top header, zero here as on the discs.
    assert_eq!(r.u32_at(0x1C).unwrap(), 0);

    assert_eq!(
        r.u32_at(data_header::AT + data_header::NODE_COUNT).unwrap(),
        2
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::NODE_LIST_PTR)
            .unwrap(),
        0x20
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::ENTRY_COUNT)
            .unwrap(),
        7
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::ENTRY_LIST_PTR)
            .unwrap(),
        0x40
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::STRING_POOL_SIZE)
            .unwrap(),
        0x20
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::STRING_POOL_PTR)
            .unwrap(),
        0xE0
    );
    assert_eq!(
        r.u16_at(data_header::AT + data_header::NEXT_FREE_ID)
            .unwrap(),
        2
    );
    assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 0);

    // The root is `ROOT` whatever its name; other nodes uppercase theirs.
    assert_eq!(&data[NODES..NODES + 4], b"ROOT");
    assert_eq!(r.u32_at(NODES + node::NAME).unwrap(), 5);
    assert_eq!(
        r.u16_at(NODES + node::NAME_HASH).unwrap(),
        name_hash(b"root")
    );
    assert_eq!(r.u16_at(NODES + node::ENTRY_COUNT).unwrap(), 4);
    assert_eq!(r.u32_at(NODES + node::FIRST_ENTRY).unwrap(), 0);
    let sub = NODES + node::LEN;
    assert_eq!(&data[sub..sub + 4], b"SUB ");
    assert_eq!(r.u32_at(sub + node::NAME).unwrap(), 16);
    assert_eq!(r.u16_at(sub + node::ENTRY_COUNT).unwrap(), 3);
    assert_eq!(r.u32_at(sub + node::FIRST_ENTRY).unwrap(), 4);

    // Root's entries: `a.bin`, `sub`, then `.` and `..` last, the order
    // every retail directory uses. Directories share the id that is no
    // id, and the root's `..` points at nothing. Ids are handed out
    // lowest first as files are met, not by entry index: a.bin claims 0,
    // and b.bin claims 1 rather than the 4 its entry sits at.
    let entry = |index: usize| {
        let at = ENTRIES + index * entry::LEN;
        (
            r.u16_at(at).unwrap(),
            r.u32_at(at + entry::FLAGS_AND_NAME).unwrap(),
            r.u32_at(at + entry::DATA_OR_NODE).unwrap(),
            r.u32_at(at + entry::DATA_SIZE).unwrap(),
        )
    };
    assert_eq!(entry(0), (0, 0x11 << 24 | 10, 0, 5));
    assert_eq!(entry(1), (0xFFFF, 0x02 << 24 | 16, 1, 0x10));
    assert_eq!(entry(2), (0xFFFF, 0x02 << 24, 0, 0x10));
    assert_eq!(entry(3), (0xFFFF, 0x02 << 24 | 2, u32::MAX, 0x10));
    assert_eq!(entry(4), (1, 0x11 << 24 | 20, 0x20, 3));
    assert_eq!(entry(5), (0xFFFF, 0x02 << 24, 1, 0x10));
    assert_eq!(entry(6), (0xFFFF, 0x02 << 24 | 2, 0, 0x10));
    assert_eq!(
        r.u16_at(ENTRIES + entry::NAME_HASH).unwrap(),
        name_hash(b"a.bin")
    );

    // The pool: dots once up front, then each directory's name followed by
    // its files' names, zero padded out to alignment.
    assert_eq!(
        &data[STRINGS..FILE_DATA],
        b".\0..\0root\0a.bin\0sub\0b.bin\0\0\0\0\0\0\0"
    );

    // Member bytes in entry order, each padded out to 0x20.
    assert_eq!(&data[FILE_DATA..FILE_DATA + 5], b"AAAAA");
    assert_eq!(&data[FILE_DATA + 0x20..FILE_DATA + 0x23], b"BBB");
    assert!(data[FILE_DATA + 0x23..].iter().all(|&byte| byte == 0));
}

/// The fidelity contract: what comes out goes back in and reproduces the
/// bytes, and what was packed reads back as it was given.
///
/// Both ids come back as they were stored: 0 and 1, the order the fixture's
/// two files were met in, not the entry indices they sit at.
#[test]
fn round_trips_byte_for_byte() {
    let data = archive();
    let opened = unpack(&data).unwrap();
    assert_eq!(opened.root, "root");

    let listed: Vec<_> = opened
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.data, file.id, file.preload))
        .collect();
    assert_eq!(
        listed,
        [
            ("a.bin", b"AAAAA".as_slice(), Some(0), Preload::Mram),
            ("sub/b.bin", b"BBB".as_slice(), Some(1), Preload::Mram),
        ]
    );

    assert_eq!(pack(&opened).unwrap(), data);
}

#[test]
fn an_empty_archive_still_has_its_root() {
    let data = pack(&Archive {
        root: "bmgres99".into(),
        files: Vec::new(),
        ..Default::default()
    })
    .unwrap();
    let opened = unpack(&data).unwrap();
    assert_eq!(opened.root, "bmgres99");
    assert!(opened.files.is_empty());
    assert_eq!(pack(&opened).unwrap(), data);
}

/// Nodes are numbered depth first, children in sibling order, so `a` and
/// everything under it is numbered before `b` is reached. Breadth first
/// would have put both `sub` directories after `b`, and the two `sub`s are
/// separate directories because a name is only ever looked up among one
/// parent's children.
#[test]
fn nested_directories_are_numbered_depth_first() {
    let files: Vec<_> = [
        ("a/x.bin", b"X"),
        ("a/sub/y.bin", b"Y"),
        ("b/sub/z.bin", b"Z"),
    ]
    .iter()
    .map(|(path, data)| File {
        path: (*path).into(),
        data: data.as_slice(),
        ..Default::default()
    })
    .collect();
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let r = Reader::new(&data);

    // Five directories, and the runs of entries they own: each node's
    // children, then its own `.` and `..`, laid out in node order.
    assert_eq!(
        r.u32_at(data_header::AT + data_header::NODE_COUNT).unwrap(),
        5
    );
    assert_eq!(
        r.u32_at(data_header::AT + data_header::ENTRY_COUNT)
            .unwrap(),
        17
    );

    let nodes = data_header::AT + data_header::LEN;
    let node = |index: usize| {
        let at = nodes + index * node::LEN;
        (
            &data[at..at + 4],
            r.u16_at(at + node::ENTRY_COUNT).unwrap(),
            r.u32_at(at + node::FIRST_ENTRY).unwrap(),
        )
    };
    assert_eq!(node(0), (b"ROOT".as_slice(), 4, 0));
    assert_eq!(node(1), (b"A   ".as_slice(), 4, 4));
    assert_eq!(node(2), (b"SUB ".as_slice(), 3, 8));
    assert_eq!(node(3), (b"B   ".as_slice(), 3, 11));
    assert_eq!(node(4), (b"SUB ".as_slice(), 3, 14));

    // A nested directory's `..` names its own parent rather than the root:
    // it is the last entry of its run, and both `sub` nodes have one.
    let entries = data_header::AT
        + r.u32_at(data_header::AT + data_header::ENTRY_LIST_PTR)
            .unwrap() as usize;
    let parent_of = |entry: usize| {
        r.u32_at(entries + entry * entry::LEN + entry::DATA_OR_NODE)
            .unwrap()
    };
    assert_eq!(parent_of(10), 1);
    assert_eq!(parent_of(16), 3);

    // Which is the tree the paths come back as, `z.bin` under `b/sub`
    // rather than under the `sub` that already existed.
    let opened = unpack(&data).unwrap();
    let paths: Vec<_> = opened.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["a/x.bin", "a/sub/y.bin", "b/sub/z.bin"]);
    assert_eq!(pack(&opened).unwrap(), data);
}

/// A flat archive comes out numbered the way the format's own convention
/// would have numbered it, every file's id its own entry index, so the sync
/// flag goes out set and the counter counts entries rather than ids. Both
/// ends derive that counter the same way, which is what leaves nothing for
/// [`Archive::next_free_id`] to carry.
#[test]
fn ids_that_match_their_entries_set_the_sync_flag() {
    let files = vec![
        File {
            path: "a.bin".into(),
            data: b"AAAAA",
            ..Default::default()
        },
        File {
            path: "b.bin".into(),
            data: b"BBB",
            ..Default::default()
        },
    ];
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let r = Reader::new(&data);

    assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 1);
    // The two files, then the root's `.` and `..`.
    assert_eq!(
        r.u32_at(data_header::AT + data_header::ENTRY_COUNT)
            .unwrap(),
        4
    );
    assert_eq!(
        r.u16_at(data_header::AT + data_header::NEXT_FREE_ID)
            .unwrap(),
        4
    );

    let opened = unpack(&data).unwrap();
    assert_eq!(opened.files[0].id, Some(0));
    assert_eq!(opened.files[1].id, Some(1));
    assert!(opened.next_free_id.is_none());
    assert_eq!(pack(&opened).unwrap(), data);
}

/// A carried id that no longer matches its entry index clears the sync
/// flag, and the next-free counter follows the ids instead of the count.
#[test]
fn carried_ids_clear_the_sync_flag() {
    let mut files = fixture();
    files[1].id = Some(9);
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let r = Reader::new(&data);
    assert_eq!(data[data_header::AT + data_header::SYNCED_IDS], 0);
    assert_eq!(
        r.u16_at(data_header::AT + data_header::NEXT_FREE_ID)
            .unwrap(),
        10
    );
    assert_eq!(unpack(&data).unwrap().files[1].id, Some(9));
}

/// A fallback id must never repeat one the caller already carried over,
/// even one that happens to sit at another file's entry index: `a.bin`
/// takes the id `sub/b.bin`'s entry index would otherwise have handed
/// out, so the fallback has to look elsewhere and lands on 0, the lowest
/// id nothing else claims.
#[test]
fn a_fallback_id_never_collides_with_a_carried_one() {
    let mut files = fixture();
    files[0].id = Some(4);
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let opened = unpack(&data).unwrap();
    assert_eq!(opened.files[0].id, Some(4));
    assert_eq!(opened.files[1].id, Some(0));
}

/// A counter that is not what the ids come to is the archive's own, and a
/// few were stored that way. Nothing else can bring it back, so it is
/// carried, and a counter that is what they come to is not.
#[test]
fn a_counter_of_its_own_is_carried() {
    let mut data = archive();
    assert!(unpack(&data).unwrap().next_free_id.is_none());

    let derived = Reader::new(&data)
        .u16_at(data_header::AT + data_header::NEXT_FREE_ID)
        .unwrap();
    let stored = derived + 3;
    data[data_header::AT + data_header::NEXT_FREE_ID
        ..data_header::AT + data_header::NEXT_FREE_ID + 2]
        .copy_from_slice(&stored.to_be_bytes());

    let opened = unpack(&data).unwrap();
    assert_eq!(opened.next_free_id, Some(stored));
    assert_eq!(pack(&opened).unwrap(), data);
}

/// The compression bits restate what the file's bytes are, so Yaz0 data
/// is marked compressed no matter what the caller thinks it is.
#[test]
fn compression_bits_follow_the_file_bytes() {
    let mut files = fixture();
    files[0].data = b"Yaz0 in shape only";
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(data[ENTRIES + entry::FLAGS_AND_NAME], 0x95);
}

/// The two sizes cover one run each, so they only mean anything with the
/// memories in order. Here the ARAM file is the later of the two, which
/// puts its bytes second, exactly what the sizes then claim.
#[test]
fn preload_totals_split_by_memory() {
    let mut files = fixture();
    files[1].preload = Preload::Aram;
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let r = Reader::new(&data);
    assert_eq!(r.u32_at(top_header::MRAM_SIZE).unwrap(), 0x20);
    assert_eq!(r.u32_at(top_header::ARAM_SIZE).unwrap(), 0x20);
    assert_eq!(unpack(&data).unwrap().files[1].preload, Preload::Aram);
}

/// Turn that list around and there is no pair of sizes that describes it,
/// so it is refused instead of packed into something the game would slice
/// down the middle of a file.
#[test]
fn refuses_files_out_of_memory_order() {
    let mut files = fixture();
    files[0].preload = Preload::Aram;
    let result = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    });
    assert!(matches!(result, Err(Error::Ungrouped)));
}

/// The order checked is the data section's, not the file list's, and the
/// two come apart: entries go out directory by directory, so `a.bin` is
/// written before `sub/b.bin` however the list is arranged. Marking
/// `a.bin` for ARAM is out of order despite it being second in the list,
/// and marking `sub/b.bin` is in order despite it being first.
#[test]
fn memory_order_follows_the_data_section() {
    let listed = vec![fixture()[1].clone(), fixture()[0].clone()];

    let mut files = listed.clone();
    files[1].preload = Preload::Aram;
    assert!(matches!(
        pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        }),
        Err(Error::Ungrouped)
    ));

    let mut files = listed;
    files[0].preload = Preload::Aram;
    let data = pack(&Archive {
        root: "root".into(),
        files,
        ..Default::default()
    })
    .unwrap();
    let r = Reader::new(&data);
    assert_eq!(r.u32_at(top_header::MRAM_SIZE).unwrap(), 0x20);
    assert_eq!(r.u32_at(top_header::ARAM_SIZE).unwrap(), 0x20);
}

#[test]
fn decodes_shift_jis_names() {
    let mut w = Writer::from(archive());
    // Halfwidth katakana RI, one byte in Shift-JIS.
    w.u8_at(STRINGS + NAME_A, 0xD8);
    w.u16_at(ENTRIES + entry::NAME_HASH, name_hash(b"\xD8.bin"));
    let data = w.finish();
    let opened = unpack(&data).unwrap();
    assert_eq!(opened.files[0].path, "ﾘ.bin");
    // And the trip back spells it in Shift-JIS again.
    assert_eq!(pack(&opened).unwrap(), data);
}

#[test]
fn rejects_other_data() {
    assert!(matches!(unpack(b"Yaz0...."), Err(Error::NotRarc)));
}

/// A truncated archive no longer matches its own size field, and is turned
/// away before any record read walks off the end.
#[test]
fn rejects_a_truncated_archive() {
    let data = archive();
    assert!(matches!(
        unpack(&data[..data.len() - 4]),
        Err(Error::Corrupt(_))
    ));
}

/// An archive claiming no directories at all has nothing the walk could
/// start from, and says so rather than complaining about node 0 missing
/// once it gets there. The message is what the match names: without the
/// check up front the walk refuses this archive too, so matching only the
/// variant would pass either way.
#[test]
fn rejects_an_archive_with_no_nodes() {
    let mut w = Writer::from(archive());
    w.u32_at(data_header::AT + data_header::NODE_COUNT, 0);
    let data = w.finish();
    assert!(matches!(
        unpack(&data),
        Err(Error::Corrupt("there is no root directory"))
    ));
}

/// Either nonsense count is caught up front, before the walk takes it as a
/// vector length. The complaint is matched too, since a count let through
/// here is still refused later, only after that allocation.
#[test]
fn rejects_counts_that_cannot_fit() {
    let counts = [
        (
            data_header::NODE_COUNT,
            "more directories than the archive could hold",
        ),
        (
            data_header::ENTRY_COUNT,
            "more entries than the archive could hold",
        ),
    ];
    for (field, complaint) in counts {
        let mut w = Writer::from(archive());
        w.u32_at(data_header::AT + field, u32::MAX);
        let data = w.finish();
        assert!(
            matches!(unpack(&data), Err(Error::Corrupt(message)) if message == complaint),
            "{complaint}"
        );
    }
}

#[test]
fn rejects_a_directory_claiming_missing_entries() {
    let mut w = Writer::from(archive());
    w.u16_at(NODES + node::ENTRY_COUNT, 100);
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

/// A cycle marks the archive corrupt rather than producing a partial
/// listing.
#[test]
fn a_directory_cycle_is_refused() {
    let mut w = Writer::from(archive());
    // Aim `sub`'s entry back at the root's node.
    w.u32_at(ENTRIES + entry::LEN + entry::DATA_OR_NODE, 0);
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

#[test]
fn a_dangling_directory_is_refused() {
    let mut w = Writer::from(archive());
    w.u32_at(ENTRIES + entry::LEN + entry::DATA_OR_NODE, 9);
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

#[test]
fn rejects_a_name_with_a_separator() {
    let mut w = Writer::from(archive());
    w.u8_at(STRINGS + NAME_A + 1, b'/');
    w.u16_at(ENTRIES + entry::NAME_HASH, name_hash(b"a/bin"));
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::UnusableName(_))));
}

#[test]
fn rejects_a_name_that_is_not_shift_jis() {
    let mut w = Writer::from(archive());
    // A lead byte with no trail byte after it.
    w.u8_at(STRINGS + NAME_A, 0x85);
    w.u16_at(ENTRIES + entry::NAME_HASH, name_hash(b"\x85.bin"));
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

#[test]
fn rejects_a_wrong_name_hash() {
    let mut w = Writer::from(archive());
    w.u16_at(ENTRIES + entry::NAME_HASH, 0xBEEF);
    let data = w.finish();
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

#[test]
fn rejects_a_file_marked_for_no_memory() {
    let mut data = archive();
    data[ENTRIES + entry::FLAGS_AND_NAME] = 0x01;
    assert!(matches!(unpack(&data), Err(Error::Corrupt(_))));
}

/// A path that could climb, or a component nothing could be named by, is
/// refused rather than packed into something the game would misread.
#[test]
fn refuses_an_unusable_path() {
    for path in ["sub/../b.bin", "", "sub//b.bin", "."] {
        let files = vec![File {
            path: path.into(),
            data: b"",
            id: None,
            preload: Preload::Mram,
        }];
        let result = pack(&Archive {
            root: "root".into(),
            files,
            ..Default::default()
        });
        assert!(matches!(result, Err(Error::UnusableName(_))), "{path}");
    }
}
