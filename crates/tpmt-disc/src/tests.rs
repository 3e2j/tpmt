//! Whole-image tests. Everything here goes in through `Disc::open` on a real
//! file, because that is the only way the positional reads are exercised at all.

use std::mem::offset_of;

use tpmt_bytes::{Be32, Layout as _, view_at_mut};

use crate::{Disc, Entry, Error, Item, Layout, Metadata, Result, Span, ciso, fst, sys};

// The preamble positions are fixed by the format, the rest is packed in behind
// it so a test image is kilobytes rather than 1.4 GB. `u32`, as the fields
// holding them are.
const APPLOADER_LEN: u32 = 0x40;
const DOL_OFFSET: u32 = 0x2480;
const DOL_LEN: u32 = 0x230;
const FST_OFFSET: u32 = 0x2700;
const DATA_OFFSET: u32 = 0x2800;
const IMAGE_LEN: u32 = 0x2830;

// Positions in the name pool of `\0a.bin\0sub\0b.bin\0empty\0c.bin\0`.
const NAME_ROOT: u32 = 0x00;
const NAME_A: u32 = 0x01;
const NAME_SUB: u32 = 0x07;
const NAME_B: u32 = 0x0B;
const NAME_EMPTY: u32 = 0x11;
const NAME_C: u32 = 0x17;
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

/// Where the title field starts, on the disc and in the boot header alike.
const TITLE: usize = offset_of!(sys::BootBin, authored.title);

/// A disc position as an index into a fixture, which is a few kilobytes.
fn index(position: u64) -> usize {
    usize::try_from(position).expect("a fixture fits in memory")
}

fn put32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// Writes one file table record. The last two fields mean different things
/// either side of the directory flag, so a caller sets them directly.
fn put_fst(data: &mut [u8], entry: usize, dir: bool, name: u32, target: u32, end_or_size: u32) {
    let at = FST_OFFSET as usize + entry * fst::Record::LEN;
    *view_at_mut(data, at).unwrap() = fst::Record {
        flags: if dir { fst::DIRECTORY_TYPE } else { 0 },
        name: fst::name_field(name).unwrap(),
        offset_or_parent: Be32::new(target),
        end_or_size: Be32::new(end_or_size),
    };
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
    data[offset_of!(sys::BootBin, authored.disc_number)] = 3;
    data[offset_of!(sys::BootBin, authored.revision)] = 2;
    data[offset_of!(sys::BootBin, authored.audio_streaming)] = 1;
    data[offset_of!(sys::BootBin, authored.stream_buffer_size)] = 10;
    data[TITLE..TITLE + 5].copy_from_slice(b"title");
    put32(
        &mut data,
        offset_of!(sys::BootBin, authored.magic),
        sys::MAGIC,
    );

    let bi2 = |field: usize| index(sys::BI2_OFFSET) + field;
    put32(
        &mut data,
        bi2(offset_of!(sys::Bi2Bin, simulated_memory_size)),
        0x0180_0000,
    );
    put32(&mut data, bi2(offset_of!(sys::Bi2Bin, debug_flag)), 2);
    put32(&mut data, bi2(offset_of!(sys::Bi2Bin, country)), 1);
    put32(&mut data, bi2(offset_of!(sys::Bi2Bin, unknown_1c)), 4);
    put32(&mut data, bi2(offset_of!(sys::Bi2Bin, unknown_20)), 5);
    put32(&mut data, bi2(offset_of!(sys::Bi2Bin, pad_spec)), 6);

    put32(&mut data, offset_of!(sys::BootBin, dol_offset), DOL_OFFSET);
    put32(&mut data, offset_of!(sys::BootBin, fst_offset), FST_OFFSET);
    let fst_len = ENTRY_COUNT as usize * fst::Record::LEN + NAME_POOL.len();
    assert_eq!(
        fst_len, FST_LEN as usize,
        "the derived values below assume this"
    );
    put32(&mut data, offset_of!(sys::BootBin, fst_size), FST_LEN);
    put32(&mut data, offset_of!(sys::BootBin, fst_max_size), FST_LEN);

    // The layout values, which the reader checks rather than keeps.
    put32(
        &mut data,
        offset_of!(sys::BootBin, debug_monitor),
        APPLOADER_LEN,
    );
    put32(
        &mut data,
        offset_of!(sys::BootBin, debug_monitor_address),
        sys::DEBUG_MONITOR_ADDRESS,
    );
    put32(
        &mut data,
        offset_of!(sys::BootBin, fst_address),
        FST_ADDRESS,
    );
    put32(
        &mut data,
        offset_of!(sys::BootBin, user_position),
        USER_POSITION,
    );
    put32(
        &mut data,
        offset_of!(sys::BootBin, user_length),
        USER_LENGTH,
    );

    // 0x20 of header, then the two halves the apploader reports.
    let apploader = index(sys::APPLOADER_OFFSET);
    put32(
        &mut data,
        apploader + offset_of!(sys::ApploaderHeader, size),
        0x10,
    );
    put32(
        &mut data,
        apploader + offset_of!(sys::ApploaderHeader, trailer_size),
        0x10,
    );

    // Two sections, the later one nearer the front, so a length taken from the
    // last section rather than the furthest would come out short.
    let dol = DOL_OFFSET as usize;
    put32(
        &mut data,
        dol + offset_of!(sys::DolHeader, section_offsets),
        0x200,
    );
    put32(
        &mut data,
        dol + offset_of!(sys::DolHeader, section_sizes),
        0x30,
    );
    put32(
        &mut data,
        dol + offset_of!(sys::DolHeader, section_offsets) + 8,
        0x100,
    );
    put32(
        &mut data,
        dol + offset_of!(sys::DolHeader, section_sizes) + 8,
        0x40,
    );

    put_fst(&mut data, 0, true, NAME_ROOT, 0, ENTRY_COUNT);
    put_fst(&mut data, 1, false, NAME_A, DATA_OFFSET, 4);
    put_fst(&mut data, 2, true, NAME_SUB, 0, 4);
    put_fst(&mut data, 3, false, NAME_B, DATA_OFFSET + 0x10, 5);
    put_fst(&mut data, 4, true, NAME_EMPTY, 0, 5);
    put_fst(&mut data, 5, false, NAME_C, DATA_OFFSET + 0x20, 6);

    let pool = FST_OFFSET as usize + ENTRY_COUNT as usize * fst::Record::LEN;
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
            span: Span {
                offset: 0x2810,
                size: 5
            },
            ..
        }
    ));
}

/// [`disc`] with distinct bytes in each file, so none can pass for zero fill or
/// another file.
fn disc_with_contents() -> Vec<u8> {
    let mut data = disc();
    let at = DATA_OFFSET as usize;
    data[at..at + 4].copy_from_slice(b"aaaa");
    data[at + 0x10..at + 0x15].copy_from_slice(b"bbbbb");
    data[at + 0x20..at + 0x26].copy_from_slice(b"cccccc");
    data
}

/// Streams `data` and checks the last files it hands out.
fn assert_streams(data: &[u8], last: &[(&str, &[u8])]) {
    let disc = open(data).unwrap();
    let entries = disc.entries().unwrap();
    let files = disc.stream(&entries).collect::<Result<Vec<_>>>().unwrap();
    let files: Vec<_> = files
        .iter()
        .map(|(path, bytes)| (*path, bytes.as_slice()))
        .collect();
    assert_eq!(files[files.len() - last.len()..], *last);
}

/// One pass hands out every file nearest first.
#[test]
fn a_stream_hands_out_files_in_offset_order() {
    assert_streams(
        &disc_with_contents(),
        &[
            ("files/a.bin", b"aaaa"),
            ("files/sub/b.bin", b"bbbbb"),
            ("files/c.bin", b"cccccc"),
        ],
    );
}

/// A file table can point one file into another's bytes. Both files still
/// come out whole, and the disc's order wins over the table's.
#[test]
fn a_stream_hands_out_overlapping_files_whole() {
    let mut data = disc_with_contents();
    put_fst(&mut data, 5, false, NAME_C, DATA_OFFSET + 2, 6);
    assert_streams(
        &data,
        &[
            ("files/a.bin", b"aaaa"),
            ("files/c.bin", b"aa\0\0\0\0"),
            ("files/sub/b.bin", b"bbbbb"),
        ],
    );
}

/// Neither length is stored anywhere on the disc, so both come out of their own
/// headers. Getting either wrong truncates a plausible looking file.
#[test]
fn preamble_lengths_come_out_of_their_own_headers() {
    let disc = open(&disc()).unwrap();
    let entries = disc.entries().unwrap();

    let sizes: Vec<(&str, u64)> = entries[..SYS_ENTRIES]
        .iter()
        .map(|entry| {
            let span = entry.span().expect("the preamble is all files");
            (entry.path(), span.size)
        })
        .collect();

    assert_eq!(
        sizes,
        [
            // 0x20 of header the two reported halves do not count.
            ("sys/apploader.img", APPLOADER_LEN as u64),
            // The furthest section reaches 0x200 + 0x30, not the 0x100 + 0x40 of
            // the one declared last.
            ("sys/main.dol", DOL_LEN as u64),
        ]
    );
}

/// The file table is the part most able to be wrong while still parsing. Each of
/// these would put bytes somewhere nobody asked for.
#[test]
fn rejects_a_file_table_that_lies() {
    /// One edit that turns a good disc into a broken one.
    type Corruption = fn(&mut Vec<u8>);

    // Where `a.bin` sits in the name pool, for the cases that rewrite it.
    const NAME: usize = (FST_OFFSET + 72 + NAME_A) as usize;

    let corrupt = |edit: Corruption| {
        let mut data = disc();
        edit(&mut data);
        open(&data).and_then(|disc| disc.entries()).unwrap_err()
    };

    let cases: [(&str, Corruption); 6] = [
        // The root says how long the table is, so one that is not a directory
        // leaves the walk unbounded.
        ("the root is not a directory", |data| {
            put32(data, FST_OFFSET as usize, 0);
        }),
        // Ending at or before the entry announcing it, and past the table.
        ("a subtree that ends behind itself", |data| {
            put_fst(data, 2, true, NAME_SUB, 0, 2);
        }),
        ("a subtree that outlives the table", |data| {
            put_fst(data, 2, true, NAME_SUB, 0, ENTRY_COUNT + 1);
        }),
        // Names that are a path rather than one component of one.
        ("a name that climbs out", |data| {
            data[NAME..][..5].copy_from_slice(b"../x\0");
        }),
        ("a name that is the directory", |data| {
            data[NAME..][..2].copy_from_slice(b".\0");
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
    data[(FST_OFFSET + 72 + NAME_A) as usize..][..5].copy_from_slice(b"\x83\x5Cin\0");

    let disc = open(&data).unwrap();
    assert_eq!(paths(&disc)[SYS_ENTRIES], "files/ソin");
}

/// A file's length is an unchecked u32 from the table and the buffer is sized
/// from it before any read happens. Left alone, a corrupt one asks for 4 GB.
#[test]
fn refuses_to_read_past_the_end_of_the_image() {
    let mut data = disc();
    put_fst(&mut data, 1, false, NAME_A, DATA_OFFSET, u32::MAX);
    let disc = open(&data).unwrap();

    let entries = disc.entries().expect("the table itself is still fine");
    let span = entries[SYS_ENTRIES].span().expect("a.bin is a file");
    assert!(matches!(disc.read(span), Err(Error::OutOfBounds { .. })));
}

/// A title long enough to fill its field leaves no terminator, so the read has
/// to stop at the end of the field rather than at the next zero. Nothing past
/// the field could stand in for one anyway: it is reserved, and a disc with
/// bytes in there is refused outright.
#[test]
fn the_title_stops_at_the_end_of_its_field() {
    let mut data = disc();
    data[TITLE..TITLE + sys::TITLE_LEN].fill(b'A');

    let disc = open(&data).unwrap();
    assert_eq!(disc.metadata().boot.title, "A".repeat(sys::TITLE_LEN));
}

/// A lead byte with nothing after it, the same corruption the file table test
/// uses, but through the header's own decoder.
#[test]
fn refuses_a_title_that_is_not_text() {
    let mut data = disc();
    data[TITLE + 4] = 0x93;

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
    assert_eq!(disc.len(), IMAGE_LEN as u64);

    let mut wii = disc_without_magic();
    put32(
        &mut wii,
        offset_of!(sys::BootBin, authored.wii_magic),
        sys::WII_MAGIC,
    );
    assert!(matches!(open(&wii), Err(Error::WiiDisc)));

    assert!(matches!(open(&disc_without_magic()), Err(Error::NotADisc)));
}

/// Positions spelled out here rather than taken from `sys`, so the rebuild is
/// checked against them, not agreed with.
#[test]
fn refuses_a_preamble_it_would_not_rebuild() {
    const BOOT_RESERVED: [(usize, usize); 4] =
        [(0x0A, 0x1C), (0x60, 0x400), (0x408, 0x420), (0x43C, 0x440)];
    const BOOT_DERIVED: [usize; 6] = [0x400, 0x404, 0x42C, 0x430, 0x434, 0x438];
    const BI2_RESERVED: [(usize, usize); 4] = [
        (0x440, 0x444),
        (0x448, 0x44C),
        (0x450, 0x458),
        (0x468, 0x2440),
    ];

    let refused = |at: usize, region: &str| {
        let mut data = disc();
        data[at] ^= 0xFF;
        match open(&data).err() {
            Some(Error::PreambleWouldChange {
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
    for at in BOOT_DERIVED {
        refused(at, "the boot header");
    }
    refused(TITLE + 6, "the boot header");
    refused(TITLE + sys::TITLE_LEN - 1, "the boot header");
}

/// Wraps an image in a CISO, leaving out every block that is all zeros, which is
/// what the real thing does to the fill.
fn ciso_file(image: &[u8], block_size: u32) -> Vec<u8> {
    let mut out = vec![0u8; index(ciso::HEADER_LEN)];
    out[..4].copy_from_slice(ciso::MAGIC);
    out[ciso::BLOCK_SIZE_FIELD..][..4].copy_from_slice(&block_size.to_le_bytes());

    for (number, block) in image.chunks(block_size as usize).enumerate() {
        if block.iter().all(|&b| b == 0) {
            continue;
        }
        out[ciso::MAP_OFFSET + number] = ciso::USED;
        out.extend_from_slice(block);
    }
    out
}

/// A container is only the image with holes in it, so it has to read back as the
/// image. The block after the hole is the one that catches a lookup going by
/// block number instead of counting the blocks actually stored.
#[test]
fn a_container_reads_as_the_image_inside_it() {
    const BLOCK: u32 = ciso::MIN_BLOCK_SIZE;

    let mut image = disc();
    image.resize(BLOCK as usize * 3, 0);
    image[BLOCK as usize * 2..][..8].copy_from_slice(b"edgecase");

    let container = ciso_file(&image, BLOCK);
    assert_eq!(
        container.len() as u64,
        ciso::HEADER_LEN + BLOCK as u64 * 2,
        "the empty block was stored anyway"
    );

    let raw = open(&image).unwrap();
    let packed = open(&container).unwrap();
    assert_eq!(packed.len(), raw.len());
    assert_eq!(paths(&packed), paths(&raw));

    // Out of the hole and into the block behind it.
    let span = Span {
        offset: BLOCK as u64 * 2 - 4,
        size: 12,
    };
    assert_eq!(packed.read(span).unwrap(), raw.read(span).unwrap());
}

#[test]
fn rejects_a_container_that_is_not_one() {
    let image = disc();
    let block = ciso::MIN_BLOCK_SIZE;

    let mut small = ciso_file(&image, block);
    small[ciso::BLOCK_SIZE_FIELD..][..4].copy_from_slice(&(block / 2).to_le_bytes());
    assert!(matches!(open(&small), Err(Error::CorruptHeader(_))));

    // The map says a block is there and the file it would be in stops short.
    let mut short = ciso_file(&image, block);
    short.truncate(index(ciso::HEADER_LEN));
    assert!(matches!(open(&short), Err(Error::CorruptHeader(_))));

    let mut nonsense = ciso_file(&image, block);
    nonsense[ciso::MAP_OFFSET] = 2;
    assert!(matches!(open(&nonsense), Err(Error::CorruptHeader(_))));
}

fn disc_without_magic() -> Vec<u8> {
    let mut data = disc();
    put32(&mut data, offset_of!(sys::BootBin, authored.magic), 0);
    data
}

// Building an image back.

/// A project on its way to an image: every path it holds, files carrying their
/// bytes and directories carrying nothing.
type Project<'a> = [(&'a str, Option<&'a [u8]>)];

/// The two preamble files a build hands over, lifted off the fixture so their
/// own headers still say what they are.
fn preamble() -> (Vec<u8>, Vec<u8>) {
    let data = disc();
    let at = |offset: usize, len: u32| data[offset..][..len as usize].to_vec();
    (
        at(index(sys::APPLOADER_OFFSET), APPLOADER_LEN),
        at(DOL_OFFSET as usize, DOL_LEN),
    )
}

/// Everything under `files/`, on top of the two the preamble needs.
fn project<'a>(
    apploader: &'a [u8],
    dol: &'a [u8],
    files: &Project<'a>,
) -> Vec<(&'a str, Option<&'a [u8]>)> {
    let mut project = vec![
        (sys::APPLOADER_PATH, Some(apploader)),
        (sys::DOL_PATH, Some(dol)),
    ];
    project.extend_from_slice(files);
    project
}

/// The project as the layout takes it: paths and sizes, no bytes.
fn items(project: &Project) -> Vec<Item> {
    project
        .iter()
        .map(|(path, data)| {
            let path = path.to_string();
            match data {
                Some(data) => Item::File {
                    path,
                    size: data.len() as u64,
                },
                None => Item::Directory { path },
            }
        })
        .collect()
}

/// Lays a project out and writes it, handing over each file in the order the
/// layout asked for it. A project the layout refuses is tested through
/// [`Layout::plan`] itself.
fn build(metadata: &Metadata, project: &Project) -> Vec<u8> {
    let layout = Layout::plan(metadata, &items(project)).unwrap();
    let mut out = Vec::new();
    let mut image = layout.write(&mut out);
    for entry in layout.entries() {
        let Entry::File { path, .. } = entry else {
            continue;
        };
        let (_, data) = project
            .iter()
            .find(|(at, _)| at == path)
            .expect("the layout only holds what it was given");
        image
            .file(data.expect("a directory is never a file"))
            .unwrap();
    }

    assert_eq!(
        image.finish().unwrap().len(),
        40,
        "a SHA-1 is 40 hex digits"
    );
    assert_eq!(out.len() as u64, layout.len());
    out
}

/// Both files come back out of a handful of values and a layout, so the fixture
/// is the one thing that says the values went back where they were read from.
#[test]
fn the_preamble_is_written_the_way_it_was_read() {
    let data = disc();
    let metadata = open(&data).unwrap().metadata().clone();

    let boot = sys::boot_bin(
        &metadata.boot,
        &sys::BootLayout {
            apploader_len: APPLOADER_LEN,
            dol_offset: DOL_OFFSET,
            fst_offset: FST_OFFSET,
            fst_len: FST_LEN,
        },
    )
    .unwrap();
    assert_eq!(boot.as_bytes(), &data[..sys::BootBin::LEN]);

    let bi2 = sys::bi2_bin(&metadata.bi2);
    let at = index(sys::BI2_OFFSET);
    assert_eq!(bi2.as_bytes(), &data[at..at + sys::Bi2Bin::LEN]);
}

/// The whole point of the writer: what goes on comes back off. Nothing about
/// the tree is kept when a disc is unpacked, so the file table here was built
/// out of nothing but the paths.
#[test]
fn a_built_image_reads_back_as_the_project_it_came_from() {
    let metadata = open(&disc()).unwrap().metadata().clone();
    let (apploader, dol) = preamble();

    // Names chosen for the order they come out in, which is by uppercased name:
    // `a.bin` ahead of `B.bin` is not the order the raw bytes sort in, and `ZA`
    // ahead of `Z_a` is not the order a case-insensitive compare gives.
    let files: &Project = &[
        ("files/B.bin", Some(b"bbbb".as_slice())),
        ("files/sub", None),
        ("files/Z_a", Some(b"zzz".as_slice())),
        ("files/sub/inner.bin", Some(b"inner".as_slice())),
        ("files/empty", None),
        ("files/a.bin", Some(b"aaaaa".as_slice())),
        ("files/ZA", Some(b"za".as_slice())),
    ];

    let image = build(&metadata, &project(&apploader, &dol, files));
    let built = open(&image).unwrap();

    assert_eq!(
        paths(&built),
        [
            "sys/apploader.img",
            "sys/main.dol",
            "files/a.bin",
            "files/B.bin",
            "files/empty",
            "files/sub",
            "files/sub/inner.bin",
            "files/ZA",
            "files/Z_a",
        ]
    );

    // Every file still holds what it was handed, at the offset the layout said.
    for entry in built.entries().unwrap() {
        let Entry::File { path, span } = entry else {
            continue;
        };
        let (_, data) = project(&apploader, &dol, files)
            .into_iter()
            .find(|(at, _)| *at == path)
            .expect("the disc reports only what was built");
        assert_eq!(built.read(span).unwrap(), data.unwrap(), "{path}");
    }

    assert_eq!(built.metadata().boot.title, metadata.boot.title);
    assert_eq!(built.metadata().bi2.pad_spec, metadata.bi2.pad_spec);
}

/// None of the positions are recorded on a disc, so an image that reopens at
/// all has already agreed with the reader about them. These are the rules
/// themselves, which the reader only ever checks against what it was given.
#[test]
fn everything_lands_where_the_layout_rules_put_it() {
    let metadata = open(&disc()).unwrap().metadata().clone();
    let (apploader, dol) = preamble();
    let files: &Project = &[
        ("files/a.bin", Some(b"a".as_slice())),
        ("files/b.bin", Some(b"bb".as_slice())),
    ];

    let image = build(&metadata, &project(&apploader, &dol, files));
    let built = open(&image).unwrap();
    let entries = built.entries().unwrap();

    let offset = |at: usize| {
        entries[at]
            .span()
            .expect("every entry here is a file")
            .offset
    };

    // The apploader is where the format fixes it, the executable on the first
    // 0x100 past its end, and the file table on the first 0x100 past that.
    assert_eq!(offset(0), sys::APPLOADER_OFFSET);
    assert_eq!(
        offset(1),
        (sys::APPLOADER_OFFSET + APPLOADER_LEN as u64).next_multiple_of(sys::PREAMBLE_ALIGN)
    );
    let fst = (offset(1) + DOL_LEN as u64).next_multiple_of(sys::PREAMBLE_ALIGN);
    assert_eq!(
        sys::fst_range(&built.read(sys::BOOT).unwrap())
            .unwrap()
            .offset,
        fst
    );

    // User data starts on the first 0x8000 past the table, and the mastering
    // fill a retail disc opens with is not put back.
    assert_eq!(offset(2), 0x8000);
    assert_eq!(offset(3), 0x8004, "files are packed on 4 byte boundaries");
    assert_eq!(
        built.len(),
        0x8006,
        "and the image stops after the last one"
    );
}

/// A project that is not a disc. Each of these would otherwise be written out
/// as an image quietly missing something.
#[test]
fn refuses_a_project_it_cannot_lay_out() {
    let metadata = open(&disc()).unwrap().metadata().clone();
    let (apploader, dol) = preamble();
    let bytes = Some(b"x".as_slice());

    let plan = |project: &Project| Layout::plan(&metadata, &items(project)).err();

    let missing = plan(&[(sys::DOL_PATH, Some(&dol))]);
    assert!(matches!(missing, Some(Error::MissingEntry(_))));

    let stray = plan(&project(&apploader, &dol, &[("sys/extra.bin", bytes)]));
    assert!(matches!(stray, Some(Error::UnknownEntry(_))));

    let orphan = plan(&project(&apploader, &dol, &[("files/gone/x.bin", bytes)]));
    assert!(matches!(orphan, Some(Error::Orphan(_))));

    // The game reads names without case, so these would shadow each other.
    let clash = plan(&project(
        &apploader,
        &dol,
        &[("files/a.bin", bytes), ("files/A.BIN", bytes)],
    ));
    assert!(matches!(clash, Some(Error::NameClash(..))));
}

/// The layout reserves a file's room from the length it was given, so bytes
/// that do not match it would push everything behind them out of place.
#[test]
fn refuses_bytes_that_are_not_the_length_the_layout_reserved() {
    let metadata = open(&disc()).unwrap().metadata().clone();
    let (apploader, dol) = preamble();
    let items = [
        Item::File {
            path: sys::APPLOADER_PATH.to_string(),
            size: apploader.len() as u64,
        },
        Item::File {
            path: sys::DOL_PATH.to_string(),
            size: dol.len() as u64,
        },
    ];

    let layout = Layout::plan(&metadata, &items).unwrap();
    let mut out = Vec::new();
    let mut image = layout.write(&mut out);
    assert!(matches!(
        image.file(&apploader[..1]),
        Err(Error::WrongSize { .. })
    ));

    // And an image that was never handed everything is not an image.
    let mut image = layout.write(&mut out);
    image.file(&apploader).unwrap();
    assert!(matches!(image.finish(), Err(Error::Mismatch(_))));
}
