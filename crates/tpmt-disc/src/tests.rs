//! Whole-image tests. Everything here goes in through `Disc::open` on a real
//! file, because that is the only way the positional reads are exercised at all.

use crate::{Disc, Entry, Error, Result, ciso, fst, sys};

// The preamble positions are fixed by the format, the rest is packed in behind
// it so a test image is kilobytes rather than 1.4 GB.
const APPLOADER_LEN: u64 = 0x40;
const DOL_OFFSET: u64 = 0x2480;
const DOL_LEN: u64 = 0x230;
const FST_OFFSET: u64 = 0x2700;
const DATA_OFFSET: u64 = 0x2800;
const IMAGE_LEN: u64 = 0x2830;

// Positions in the name pool of `\0a.bin\0sub\0b.bin\0empty\0c.bin\0`.
const NAME_ROOT: u32 = 0;
const NAME_A: u32 = 1;
const NAME_SUB: u32 = 7;
const NAME_B: u32 = 11;
const NAME_EMPTY: u32 = 17;
const NAME_C: u32 = 23;
const NAME_POOL: &[u8] = b"\0a.bin\0sub\0b.bin\0empty\0c.bin\0";

const ENTRY_COUNT: u32 = 6;
const FST_LEN: u32 = 0x65;

// What the reader works out for itself rather than keeping, spelled out here so
// a wrong rule fails rather than agreeing with itself. The file table is loaded
// as high as it fits under 0x80400000 on a 32 byte boundary; user data starts on
// the first 0x8000 past the end of the table at 0x2765, and runs to 0x57058000.
const FST_ADDRESS: u32 = 0x803F_FF80;
const USER_POSITION: u32 = 0x8000;
const USER_LENGTH: u32 = 0x5705_0000;

/// The preamble entries, which come back ahead of the file table. Only the
/// apploader and the executable are among them, so the game's own files start
/// here.
const SYS_ENTRIES: usize = 2;

fn put32(data: &mut [u8], at: u64, value: u32) {
    let at = at as usize;
    data[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// Writes one file table record. The last two fields mean different things
/// either side of the directory flag, so a caller sets them directly.
fn put_fst(data: &mut [u8], index: u32, dir: bool, name: u32, target: u32, end_or_size: u32) {
    let at = FST_OFFSET + u64::from(index) * fst::ENTRY_LEN as u64;
    let flag = if dir { fst::DIRECTORY_FLAG } else { 0 };
    put32(data, at, flag | name);
    put32(data, at + 4, target);
    put32(data, at + 8, end_or_size);
}

/// A whole disc image, holding:
///
/// ```text
/// files/a.bin
/// files/sub/b.bin
/// files/empty/
/// files/c.bin
/// ```
///
/// `empty` is the case that only survives if directories are reported.
fn disc() -> Vec<u8> {
    let mut data = vec![0u8; IMAGE_LEN as usize];

    // Every kept field gets a value of its own, so a reader constant pointing
    // at the wrong place reads some other field, or the zero fill, and fails.
    data[..6].copy_from_slice(b"GZ2E01");
    data[sys::DISC_NUMBER_OFFSET] = 3;
    data[sys::REVISION_OFFSET] = 2;
    data[sys::AUDIO_STREAMING_OFFSET] = 1;
    data[sys::STREAM_BUFFER_SIZE_OFFSET] = 10;
    data[sys::TITLE_OFFSET..sys::TITLE_OFFSET + 5].copy_from_slice(b"title");
    put32(&mut data, sys::MAGIC_OFFSET as u64, sys::MAGIC);

    let bi2 = |field: usize| sys::BI2_OFFSET + field as u64;
    put32(&mut data, bi2(sys::BI2_SIMULATED_MEMORY_SIZE), 0x0180_0000);
    put32(&mut data, bi2(sys::BI2_DEBUG_FLAG), 2);
    put32(&mut data, bi2(sys::BI2_COUNTRY), 1);
    put32(&mut data, bi2(sys::BI2_UNKNOWN_1C), 4);
    put32(&mut data, bi2(sys::BI2_UNKNOWN_20), 5);
    put32(&mut data, bi2(sys::BI2_PAD_SPEC), 6);

    put32(&mut data, sys::DOL_OFFSET_FIELD as u64, DOL_OFFSET as u32);
    put32(&mut data, sys::FST_OFFSET_FIELD as u64, FST_OFFSET as u32);
    let fst_len = ENTRY_COUNT as usize * fst::ENTRY_LEN + NAME_POOL.len();
    assert_eq!(
        fst_len as u32, FST_LEN,
        "the derived values below assume this"
    );
    put32(&mut data, sys::FST_SIZE_FIELD as u64, FST_LEN);
    put32(&mut data, sys::FST_MAX_SIZE_FIELD as u64, FST_LEN);

    // The layout values, which the reader checks rather than keeps.
    put32(
        &mut data,
        sys::DEBUG_MONITOR_FIELD as u64,
        APPLOADER_LEN as u32,
    );
    put32(
        &mut data,
        sys::DEBUG_MONITOR_ADDRESS_FIELD as u64,
        sys::DEBUG_MONITOR_ADDRESS,
    );
    put32(&mut data, sys::FST_ADDRESS_FIELD as u64, FST_ADDRESS);
    put32(&mut data, sys::USER_POSITION_FIELD as u64, USER_POSITION);
    put32(&mut data, sys::USER_LENGTH_FIELD as u64, USER_LENGTH);

    // 0x20 of header, then the two halves the apploader reports.
    put32(
        &mut data,
        sys::APPLOADER_OFFSET + sys::APPLOADER_SIZE_FIELD as u64,
        0x10,
    );
    put32(
        &mut data,
        sys::APPLOADER_OFFSET + sys::APPLOADER_TRAILER_FIELD as u64,
        0x10,
    );

    // Two sections, the later one nearer the front, so a length taken from the
    // last section rather than the furthest would come out short.
    put32(
        &mut data,
        DOL_OFFSET + sys::DOL_SECTION_OFFSETS as u64,
        0x200,
    );
    put32(&mut data, DOL_OFFSET + sys::DOL_SECTION_SIZES as u64, 0x30);
    put32(
        &mut data,
        DOL_OFFSET + sys::DOL_SECTION_OFFSETS as u64 + 8,
        0x100,
    );
    put32(
        &mut data,
        DOL_OFFSET + sys::DOL_SECTION_SIZES as u64 + 8,
        0x40,
    );

    put_fst(&mut data, 0, true, NAME_ROOT, 0, ENTRY_COUNT);
    put_fst(&mut data, 1, false, NAME_A, DATA_OFFSET as u32, 4);
    put_fst(&mut data, 2, true, NAME_SUB, 0, 4);
    put_fst(&mut data, 3, false, NAME_B, DATA_OFFSET as u32 + 0x10, 5);
    put_fst(&mut data, 4, true, NAME_EMPTY, 0, 5);
    put_fst(&mut data, 5, false, NAME_C, DATA_OFFSET as u32 + 0x20, 6);

    let pool = (FST_OFFSET + ENTRY_COUNT as u64 * fst::ENTRY_LEN as u64) as usize;
    data[pool..pool + NAME_POOL.len()].copy_from_slice(NAME_POOL);

    data
}

/// `Disc` reads positionally from a real file, so a test needs one on disk.
fn open(data: &[u8]) -> Result<Disc> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);

    let path = std::env::temp_dir().join(format!(
        "tpmt-disc-{}-{}.iso",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, data).expect("the temp directory is writable");
    let disc = Disc::open(&path);
    let _ = std::fs::remove_file(&path);
    disc
}

fn paths(disc: &Disc) -> Vec<String> {
    disc.entries()
        .unwrap()
        .iter()
        .map(|entry| entry.path().to_string())
        .collect()
}

/// Nesting is index ranges, not pointers, so leaving a directory means noticing
/// an index has passed the end it recorded. Two directories end on consecutive
/// indices here, catching a walk that leaves one at a time.
#[test]
fn walks_the_file_table_depth_first() {
    let disc = open(&disc()).unwrap();
    assert_eq!(
        paths(&disc)[SYS_ENTRIES..],
        [
            "files/a.bin",
            "files/sub",
            "files/sub/b.bin",
            "files/empty",
            "files/c.bin",
        ]
    );

    let entries = disc.entries().unwrap();
    assert!(matches!(entries[SYS_ENTRIES + 1], Entry::Directory { .. }));
    assert!(matches!(
        entries[SYS_ENTRIES + 2],
        Entry::File {
            offset: 0x2810,
            size: 5,
            ..
        }
    ));
}

/// Neither length is stored anywhere on the disc, so both come out of their own
/// headers. Getting either wrong truncates a plausible looking file.
#[test]
fn preamble_lengths_come_out_of_their_own_headers() {
    let disc = open(&disc()).unwrap();
    let entries = disc.entries().unwrap();

    let sizes: Vec<(&str, u64)> = entries[..SYS_ENTRIES]
        .iter()
        .map(|entry| match entry {
            Entry::File { path, size, .. } => (path.as_str(), *size),
            Entry::Directory { .. } => unreachable!("the preamble is all files"),
        })
        .collect();

    assert_eq!(
        sizes,
        [
            // 0x20 of header the two reported halves do not count.
            ("sys/apploader.img", APPLOADER_LEN),
            // The furthest section reaches 0x200 + 0x30, not the 0x100 + 0x40 of
            // the one declared last.
            ("sys/main.dol", DOL_LEN),
        ]
    );
}

/// The file table is the part most able to be wrong while still parsing. Each of
/// these would put bytes somewhere nobody asked for.
#[test]
fn rejects_a_file_table_that_lies() {
    /// One edit that turns a good disc into a broken one.
    type Corruption = fn(&mut Vec<u8>);

    let corrupt = |edit: Corruption| {
        let mut data = disc();
        edit(&mut data);
        open(&data).and_then(|disc| disc.entries()).unwrap_err()
    };

    // Where `a.bin` sits in the name pool, for the cases that rewrite it.
    const NAME: usize = (FST_OFFSET + 72 + NAME_A as u64) as usize;

    let cases: [(&str, Corruption); 6] = [
        // The root says how long the table is, so one that is not a directory
        // leaves the walk unbounded.
        ("the root is not a directory", |data| {
            put32(data, FST_OFFSET, 0)
        }),
        // Ending at or before the entry announcing it, and past the table.
        ("a subtree that ends behind itself", |data| {
            put_fst(data, 2, true, NAME_SUB, 0, 2)
        }),
        ("a subtree that outlives the table", |data| {
            put_fst(data, 2, true, NAME_SUB, 0, ENTRY_COUNT + 1)
        }),
        // Names that are a path rather than one component of one.
        ("a name that climbs out", |data| {
            data[NAME..][..5].copy_from_slice(b"../x\0")
        }),
        ("a name that is the directory", |data| {
            data[NAME..][..2].copy_from_slice(b".\0")
        }),
        // A lead byte with nothing that can follow it.
        ("a name that is not Shift-JIS", |data| data[NAME] = 0x93),
    ];

    for (what, edit) in cases {
        assert!(
            matches!(corrupt(edit), Error::CorruptFileTable(_)),
            "{what} was accepted"
        );
    }
}

/// `ソ` is `0x83 0x5C`, and `0x5C` alone is a path separator. Judging the raw
/// bytes throws out names that are fine once decoded.
#[test]
fn a_name_is_read_before_it_is_judged() {
    let mut data = disc();
    // Over `a.bin\0`, which has room to spare.
    data[(FST_OFFSET + 72 + NAME_A as u64) as usize..][..5].copy_from_slice(b"\x83\x5Cin\0");

    let disc = open(&data).unwrap();
    assert_eq!(paths(&disc)[SYS_ENTRIES], "files/ソin");
}

/// A file's length is an unchecked u32 from the table and the buffer is sized
/// from it before any read happens. Left alone, a corrupt one asks for 4 GB.
#[test]
fn refuses_to_read_past_the_end_of_the_image() {
    let mut data = disc();
    put_fst(&mut data, 1, false, NAME_A, DATA_OFFSET as u32, u32::MAX);
    let disc = open(&data).unwrap();

    let entries = disc.entries().expect("the table itself is still fine");
    let Entry::File { offset, size, .. } = entries[SYS_ENTRIES] else {
        unreachable!("a.bin is a file")
    };
    assert!(matches!(disc.read(offset, size), Err(Error::Read { .. })));
}

/// A title long enough to fill its field leaves no terminator, so the read has
/// to stop at the end of the field rather than at the next zero. Nothing past
/// the field could stand in for one anyway: it is reserved, and a disc with
/// bytes in there is refused outright.
#[test]
fn the_title_stops_at_the_end_of_its_field() {
    let mut data = disc();
    data[sys::TITLE_OFFSET..sys::TITLE_OFFSET + sys::TITLE_LEN].fill(b'A');

    let disc = open(&data).unwrap();
    assert_eq!(disc.metadata().boot.title, "A".repeat(sys::TITLE_LEN));
}

/// A lead byte with nothing after it, the same corruption the file table test
/// uses, but through the header's own decoder.
#[test]
fn refuses_a_title_that_is_not_text() {
    let mut data = disc();
    data[sys::TITLE_OFFSET + 4] = 0x93;

    assert!(matches!(open(&data), Err(Error::CorruptHeader(_))));
}

/// The game has a Wii print, so saying which disc this is beats refusing it as
/// unreadable.
#[test]
fn says_what_it_is_looking_at() {
    let disc = open(&disc()).unwrap();
    let meta = disc.metadata();
    assert_eq!(meta.boot.id, "GZ2E");
    assert_eq!(meta.boot.maker, "01");
    assert_eq!(meta.boot.disc_number, 3);
    assert_eq!(meta.boot.revision, 2);
    assert_eq!(meta.boot.audio_streaming, 1);
    assert_eq!(meta.boot.stream_buffer_size, 10);
    assert_eq!(meta.boot.title, "title");
    assert_eq!(meta.bi2.simulated_memory_size, 0x0180_0000);
    assert_eq!(meta.bi2.debug_flag, 2);
    assert_eq!(meta.bi2.country, 1);
    assert_eq!(meta.bi2.unknown_1c, 4);
    assert_eq!(meta.bi2.unknown_20, 5);
    assert_eq!(meta.bi2.pad_spec, 6);
    assert_eq!(disc.len(), IMAGE_LEN);

    let mut wii = disc_without_magic();
    put32(&mut wii, sys::WII_MAGIC_OFFSET as u64, sys::WII_MAGIC);
    assert!(matches!(open(&wii), Err(Error::WiiDisc)));

    assert!(matches!(open(&disc_without_magic()), Err(Error::NotADisc)));
}

/// Keeping the boot header and the disc metadata as a handful of values only
/// works while the rest of those two files is empty. One that put something
/// elsewhere is refused, rather than unpacked into a project that has quietly
/// dropped it.
///
/// The ranges are spelled out again here, first and last byte of each, so the
/// tables in `sys` are checked rather than agreed with. Positions on the disc,
/// which is what the error reports.
#[test]
fn refuses_a_preamble_it_would_not_keep_whole() {
    const BOOT_RESERVED: [(usize, usize); 4] =
        [(0x0A, 0x1C), (0x60, 0x400), (0x408, 0x420), (0x43C, 0x440)];
    const BI2_RESERVED: [(usize, usize); 4] = [
        (0x440, 0x444),
        (0x448, 0x44C),
        (0x450, 0x458),
        (0x468, 0x2440),
    ];

    let refused = |at: usize, region: &str| {
        let mut data = disc();
        data[at] = 1;
        match open(&data).err() {
            Some(Error::UnknownPreambleData {
                region: got,
                offset,
            }) if got == region && offset == at as u64 => {}
            other => panic!("{at:#x} came back as {other:?}"),
        }
    };

    for (ranges, region) in [
        (BOOT_RESERVED, "the boot header"),
        (BI2_RESERVED, "the disc metadata"),
    ] {
        for (from, to) in ranges {
            refused(from, region);
            refused(to - 1, region);
        }
    }
}

/// The addresses and offsets are worked out again rather than stored, so a disc
/// holding something else in one of them is a disc whose rule is not the one
/// here. Rebuilding it would silently move things, so it is refused instead.
#[test]
fn refuses_a_layout_it_would_not_reproduce() {
    let fields = [
        sys::DEBUG_MONITOR_FIELD,
        sys::DEBUG_MONITOR_ADDRESS_FIELD,
        sys::FST_MAX_SIZE_FIELD,
        sys::FST_ADDRESS_FIELD,
        sys::USER_POSITION_FIELD,
        sys::USER_LENGTH_FIELD,
    ];

    for at in fields {
        let mut data = disc();
        put32(&mut data, at as u64, 0xDEAD_BEEF);
        assert!(
            matches!(
                open(&data).err(),
                Some(Error::DerivedValueDiffers {
                    found: 0xDEAD_BEEF,
                    ..
                })
            ),
            "{at:#x} was accepted"
        );
    }
}

/// Wraps an image in a CISO, leaving out every block that is all zeros, which is
/// what the real thing does to the fill.
fn ciso_file(image: &[u8], block_size: usize) -> Vec<u8> {
    let mut out = vec![0u8; ciso::HEADER_LEN as usize];
    out[..4].copy_from_slice(ciso::MAGIC);
    out[ciso::BLOCK_SIZE_FIELD..][..4].copy_from_slice(&(block_size as u32).to_le_bytes());

    for (index, block) in image.chunks(block_size).enumerate() {
        if block.iter().all(|&b| b == 0) {
            continue;
        }
        out[ciso::MAP_OFFSET + index] = ciso::USED;
        out.extend_from_slice(block);
    }
    out
}

/// A container is only the image with holes in it, so it has to read back as the
/// image. The block after the hole is the one that catches a lookup going by
/// block number instead of counting the blocks actually stored.
#[test]
fn a_container_reads_as_the_image_inside_it() {
    const BLOCK: u64 = ciso::MIN_BLOCK_SIZE as u64;

    let mut image = disc();
    image.resize(BLOCK as usize * 3, 0);
    image[BLOCK as usize * 2..][..8].copy_from_slice(b"edgecase");

    let container = ciso_file(&image, BLOCK as usize);
    assert_eq!(
        container.len() as u64,
        ciso::HEADER_LEN + BLOCK * 2,
        "the empty block was stored anyway"
    );

    let raw = open(&image).unwrap();
    let packed = open(&container).unwrap();
    assert_eq!(packed.len(), raw.len());
    assert_eq!(paths(&packed), paths(&raw));

    // Out of the hole and into the block behind it.
    let (at, len) = (BLOCK * 2 - 4, 12);
    assert_eq!(packed.read(at, len).unwrap(), raw.read(at, len).unwrap());
}

#[test]
fn rejects_a_container_that_is_not_one() {
    let image = disc();
    let block = ciso::MIN_BLOCK_SIZE as usize;

    let mut small = ciso_file(&image, block);
    small[ciso::BLOCK_SIZE_FIELD..][..4].copy_from_slice(&(block as u32 / 2).to_le_bytes());
    assert!(matches!(open(&small), Err(Error::CorruptHeader(_))));

    // The map says a block is there and the file it would be in stops short.
    let mut short = ciso_file(&image, block);
    short.truncate(ciso::HEADER_LEN as usize);
    assert!(matches!(open(&short), Err(Error::CorruptHeader(_))));

    let mut nonsense = ciso_file(&image, block);
    nonsense[ciso::MAP_OFFSET] = 2;
    assert!(matches!(open(&nonsense), Err(Error::CorruptHeader(_))));
}

fn disc_without_magic() -> Vec<u8> {
    let mut data = disc();
    put32(&mut data, sys::MAGIC_OFFSET as u64, 0);
    data
}
