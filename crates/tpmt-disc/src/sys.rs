//! The preamble: the fixed run at the front of a disc that says what everything
//! else is and where it went.
//!
//! Boot header, disc metadata, apploader, executable. Only the first two sit
//! where the format fixes them. The boot header says where the other two are,
//! and neither of those records its own length anywhere but inside itself.
//!
//! The first two are almost entirely empty, and most of what they do hold is a
//! consequence of the layout rather than anything anyone chose. What is left is
//! thirteen values, so they come back as `Metadata` instead of as files. The
//! rest is either checked against the rule that produces it or checked for
//! being zero, so a disc that does not match is refused rather than quietly
//! rebuilt into something else.

use serde::{Deserialize, Serialize};
use tpmt_bytes::{Reader, Writer};

use crate::{Disc, Entry, Error, Result};

// Boot header. The magic is what makes this a GameCube disc rather than
// anything else that happens to be 1.4 GB.
pub(crate) const MAGIC: u32 = 0xC233_9F3D;
pub(crate) const MAGIC_OFFSET: usize = 0x1C;
pub(crate) const WII_MAGIC: u32 = 0x5D1C_9EA3;
pub(crate) const WII_MAGIC_OFFSET: usize = 0x18;
pub(crate) const BOOT_LEN: u64 = 0x440;

pub(crate) const ID_OFFSET: usize = 0x00;
pub(crate) const ID_LEN: usize = 4;
pub(crate) const MAKER_OFFSET: usize = 0x04;
pub(crate) const MAKER_LEN: usize = 2;
pub(crate) const DISC_NUMBER_OFFSET: usize = 0x06;
pub(crate) const REVISION_OFFSET: usize = 0x07;
pub(crate) const AUDIO_STREAMING_OFFSET: usize = 0x08;
pub(crate) const STREAM_BUFFER_SIZE_OFFSET: usize = 0x09;
pub(crate) const TITLE_OFFSET: usize = 0x20;
pub(crate) const TITLE_LEN: usize = 0x40;
/// Where the values a project keeps stop and the derived numbers start.
const AUTHORED_LEN: usize = TITLE_OFFSET + TITLE_LEN;

// The layout, all of it worked out again by a build rather than kept. `DVDBB2`
// in the SDK covers the six from 0x420 on.
pub(crate) const DEBUG_MONITOR_FIELD: usize = 0x400;
pub(crate) const DEBUG_MONITOR_ADDRESS_FIELD: usize = 0x404;
pub(crate) const DOL_OFFSET_FIELD: usize = 0x420;
pub(crate) const FST_OFFSET_FIELD: usize = 0x424;
pub(crate) const FST_SIZE_FIELD: usize = 0x428;
pub(crate) const FST_MAX_SIZE_FIELD: usize = 0x42C;
pub(crate) const FST_ADDRESS_FIELD: usize = 0x430;
pub(crate) const USER_POSITION_FIELD: usize = 0x434;
pub(crate) const USER_LENGTH_FIELD: usize = 0x438;

/// Where the debug monitor would be loaded. The same on all three prints, and
/// nothing on a retail disc reads it.
pub(crate) const DEBUG_MONITOR_ADDRESS: u32 = 0x8028_0060;
/// The file table is loaded as high as it fits under here, and the arena ends
/// where it starts.
const FST_TOP: u32 = 0x8040_0000;
/// The end of a GameCube disc's user area.
pub(crate) const USER_AREA_END: u32 = 0x5705_8000;
/// User data starts on one of these, past the file table.
const USER_ALIGN: u32 = 0x8000;
/// The executable and the file table each start on one of these, past whatever
/// the layout put in front of them.
pub(crate) const PREAMBLE_ALIGN: u64 = 0x100;

/// The stretches of the boot header that hold nothing on any of the three
/// prints. Everything outside them is either kept or checked, so a disc with
/// bytes in here is one this would not reproduce.
const BOOT_RESERVED: [(usize, usize); 4] =
    [(0x0A, 0x1C), (0x60, 0x400), (0x408, 0x420), (0x43C, 0x440)];

// Disc metadata, then the apploader, at fixed positions after the boot header.
pub(crate) const BI2_OFFSET: u64 = 0x440;
pub(crate) const BI2_LEN: u64 = 0x2000;
pub(crate) const APPLOADER_OFFSET: u64 = 0x2440;
pub(crate) const APPLOADER_HEADER_LEN: u64 = 0x20;
pub(crate) const APPLOADER_SIZE_FIELD: usize = 0x14;
pub(crate) const APPLOADER_TRAILER_FIELD: usize = 0x18;

pub(crate) const BI2_SIMULATED_MEMORY_SIZE: usize = 0x04;
pub(crate) const BI2_DEBUG_FLAG: usize = 0x0C;
pub(crate) const BI2_COUNTRY: usize = 0x18;
pub(crate) const BI2_UNKNOWN_1C: usize = 0x1C;
pub(crate) const BI2_UNKNOWN_20: usize = 0x20;
/// `__PADSpec`, which `OSInit` reads straight out of here.
pub(crate) const BI2_PAD_SPEC: usize = 0x24;

/// Everything bi2 does not use: the debug monitor size, the argument offset,
/// the two track fields, and then eight kilobytes of nothing.
const BI2_RESERVED: [(usize, usize); 4] = [
    (0x00, 0x04),
    (0x08, 0x0C),
    (0x10, 0x18),
    (0x28, BI2_LEN as usize),
];

/// Where the two preamble files land in a project. A build looks for them by
/// these names, so they are spelled once.
pub(crate) const APPLOADER_PATH: &str = "sys/apploader.img";
pub(crate) const DOL_PATH: &str = "sys/main.dol";
/// The two preamble pieces, which are not files a project holds: they are kept
/// as their values and built again. Named here because a mod that carries one
/// has to call it what a disc calls it.
pub const BOOT_PATH: &str = "sys/boot.bin";
pub const BI2_PATH: &str = "sys/bi2.bin";

// Executable. Its length is not stored anywhere, so it is whatever the furthest
// section reaches.
pub(crate) const DOL_HEADER_LEN: u64 = 0x100;
pub(crate) const DOL_SECTIONS: usize = 18;
pub(crate) const DOL_SECTION_OFFSETS: usize = 0x00;
pub(crate) const DOL_SECTION_SIZES: usize = 0x90;

/// What the preamble records that a build cannot work out for itself.
///
/// Everything else in the boot header and the disc metadata is zero, or a
/// constant, or follows from where things ended up, so a project keeps this and
/// neither file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub boot: Boot,
    pub bi2: Bi2,
}

/// Who the disc says it is. Nothing here is an address or an offset: those all
/// come back out of the layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boot {
    /// Four characters: system, game code, region. `GZ2E` and friends.
    pub id: String,
    /// Two characters naming the publisher, `01` for Nintendo.
    pub maker: String,
    pub disc_number: u8,
    /// Revision of the print, `0` for the first.
    pub revision: u8,
    /// Whether the game reads audio straight off the disc rather than through
    /// the file table. Zero on all three prints, and the library that would act
    /// on it is linked into none of them.
    pub audio_streaming: u8,
    pub stream_buffer_size: u8,
    pub title: String,
}

/// The six things eight kilobytes of disc metadata actually say.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bi2 {
    pub simulated_memory_size: u32,
    /// Read by `OSInit`. Anything under 2 is a retail console.
    pub debug_flag: u32,
    /// 0 Japan, 1 America, 2 Europe.
    pub country: u32,
    pub unknown_1c: u32,
    pub unknown_20: u32,
    /// `__PADSpec`, which decides how `OSInit` talks to the controllers.
    pub pad_spec: u32,
}

/// Refuses anything that is not a GameCube disc image.
///
/// Called before the rest of the preamble is read, so a file that is not a disc
/// says so rather than failing on a short read somewhere inside it.
pub(crate) fn identify(boot: &[u8]) -> Result<()> {
    let reader = Reader::new(boot);
    if reader.u32_at(MAGIC_OFFSET)? == MAGIC {
        return Ok(());
    }

    // Both magics sit in the same header and only one is ever set, so a Wii
    // disc can be declined by name rather than as a mystery.
    match reader.u32_at(WII_MAGIC_OFFSET)? == WII_MAGIC {
        true => Err(Error::WiiDisc),
        false => Err(Error::NotADisc),
    }
}

/// Reads the boot header, having already been told it is one by `identify`.
pub(crate) fn boot(bytes: &[u8], apploader_len: u64) -> Result<Boot> {
    let reader = Reader::new(bytes);

    for (from, to) in BOOT_RESERVED {
        let reserved = reader.slice_at(from, to - from)?;
        if let Some(at) = reserved.iter().position(|&b| b != 0) {
            return Err(Error::UnknownPreambleData {
                region: "the boot header",
                offset: (from + at) as u64,
            });
        }
    }
    check_layout(&reader, apploader_len)?;

    // A 64 byte field, only terminated when the title is short enough to leave
    // room, so the read stops at the end of the field either way.
    let title = reader.slice_at(TITLE_OFFSET, TITLE_LEN)?;
    let title = &title[..title.iter().position(|&b| b == 0).unwrap_or(title.len())];

    Ok(Boot {
        id: text(
            reader.slice_at(ID_OFFSET, ID_LEN)?,
            "the game id is not text",
        )?,
        maker: text(
            reader.slice_at(MAKER_OFFSET, MAKER_LEN)?,
            "the maker code is not text",
        )?,
        disc_number: reader.u8_at(DISC_NUMBER_OFFSET)?,
        revision: reader.u8_at(REVISION_OFFSET)?,
        audio_streaming: reader.u8_at(AUDIO_STREAMING_OFFSET)?,
        stream_buffer_size: reader.u8_at(STREAM_BUFFER_SIZE_OFFSET)?,
        title: text(title, "the title is not text")?,
    })
}

/// Checks the six header values a build works out again instead of keeping.
///
/// Nothing is stored for them, so if one is not what its rule says, the rule
/// does not hold for this disc and a build would put something else there.
///
/// The executable's and the file table's own offsets are not held to their
/// alignment rule. They are read off the disc and used as they are, and a
/// build works out fresh ones and rewrites every field that mentions them, so
/// a disc that packed its preamble differently still unpacks and rebuilds
/// whole. The six here have no such second life: they are only ever derived.
fn check_layout(reader: &Reader, apploader_len: u64) -> Result<()> {
    let fst_offset = reader.u32_at(FST_OFFSET_FIELD)?;
    let fst_len = reader.u32_at(FST_SIZE_FIELD)?;
    let user = user_position(fst_offset, fst_len);

    let derived = [
        // The mastering put the apploader's length here on all three prints,
        // whatever it meant by it, and nothing on a retail disc reads it.
        (
            DEBUG_MONITOR_FIELD,
            apploader_len as u32,
            "the debug monitor offset",
        ),
        (
            DEBUG_MONITOR_ADDRESS_FIELD,
            DEBUG_MONITOR_ADDRESS,
            "the debug monitor address",
        ),
        (
            FST_ADDRESS_FIELD,
            fst_address(fst_len),
            "the file table's load address",
        ),
        (USER_POSITION_FIELD, user, "the user area start"),
        (
            USER_LENGTH_FIELD,
            USER_AREA_END.saturating_sub(user),
            "the user area length",
        ),
        // Only ever different on a game spanning several discs, which this is
        // not, so a build has nothing to take a maximum over.
        (FST_MAX_SIZE_FIELD, fst_len, "the largest file table"),
    ];

    for (at, want, what) in derived {
        let found = reader.u32_at(at)?;
        if found != want {
            return Err(Error::DerivedValueDiffers { what, found, want });
        }
    }
    Ok(())
}

/// The file table is loaded as high as it goes, on a 32 byte boundary because
/// `DVDChangeDisk` asserts on that.
fn fst_address(fst_len: u32) -> u32 {
    FST_TOP.saturating_sub(fst_len) & !31
}

/// User data starts on the first boundary past the file table.
pub(crate) fn user_position(fst_offset: u32, fst_len: u32) -> u32 {
    fst_offset
        .saturating_add(fst_len)
        .checked_next_multiple_of(USER_ALIGN)
        .unwrap_or(0)
}

/// Reads the disc metadata, which is six fields and then eight kilobytes of
/// nothing.
pub(crate) fn bi2(bytes: &[u8]) -> Result<Bi2> {
    let reader = Reader::new(bytes);

    for (from, to) in BI2_RESERVED {
        let reserved = reader.slice_at(from, to - from)?;
        if let Some(at) = reserved.iter().position(|&b| b != 0) {
            // Reported as a position on the disc, which is where a hex editor
            // over the image will be looking.
            return Err(Error::UnknownPreambleData {
                region: "the disc metadata",
                offset: BI2_OFFSET + (from + at) as u64,
            });
        }
    }

    Ok(Bi2 {
        simulated_memory_size: reader.u32_at(BI2_SIMULATED_MEMORY_SIZE)?,
        debug_flag: reader.u32_at(BI2_DEBUG_FLAG)?,
        country: reader.u32_at(BI2_COUNTRY)?,
        unknown_1c: reader.u32_at(BI2_UNKNOWN_1C)?,
        unknown_20: reader.u32_at(BI2_UNKNOWN_20)?,
        pad_spec: reader.u32_at(BI2_PAD_SPEC)?,
    })
}

/// Writes the boot header back out: the inverse of `boot` and `check_layout`.
///
/// Seven kept values and the magic. Everything else is a run of zeros, or a
/// number that follows from where the layout put the three things the header
/// points at.
pub(crate) fn boot_bin(
    boot: &Boot,
    apploader_len: u32,
    dol_offset: u32,
    fst_offset: u32,
    fst_len: u32,
) -> Result<Vec<u8>> {
    let mut out = Writer::with_capacity(BOOT_LEN as usize);
    authored(&mut out, boot)?;

    pad_to(&mut out, DEBUG_MONITOR_FIELD);
    out.u32(apploader_len);
    out.u32(DEBUG_MONITOR_ADDRESS);

    pad_to(&mut out, DOL_OFFSET_FIELD);
    out.u32(dol_offset);
    out.u32(fst_offset);
    out.u32(fst_len);
    // Only ever larger on a game spanning several discs, and this is one disc.
    out.u32(fst_len);
    out.u32(fst_address(fst_len));
    let user = user_position(fst_offset, fst_len);
    out.u32(user);
    out.u32(USER_AREA_END.saturating_sub(user));

    pad_to(&mut out, BOOT_LEN as usize);
    Ok(out.finish())
}

/// A disc's own boot header with a project's values written into it.
///
/// Everything a person can edit sits in the first 0x60 bytes. The rest is
/// derived from a layout, and the layout a mod is installed into is not the one
/// it was built against, so the original's numbers are kept rather than
/// replaced by ours. Whatever applies the mod works those out again for the
/// disc it is writing, which is the only place they mean anything.
///
/// Which also makes this its own comparison: what comes back is the original
/// unless a value somebody edited is in it.
pub fn boot_bin_over(original: &[u8], boot: &Boot) -> Result<Vec<u8>> {
    if original.len() != BOOT_LEN as usize {
        return Err(Error::Unwritable("a boot header is 0x440 bytes"));
    }

    let mut out = Writer::with_capacity(BOOT_LEN as usize);
    authored(&mut out, boot)?;
    out.bytes(&original[AUTHORED_LEN..]);
    Ok(out.finish())
}

/// The part of the header a project keeps: seven values and the magic, and the
/// zeros between them.
fn authored(out: &mut Writer, boot: &Boot) -> Result<()> {
    out.bytes(&exactly(
        &boot.id,
        ID_LEN,
        "game id has to be four characters",
    )?);
    out.bytes(&exactly(
        &boot.maker,
        MAKER_LEN,
        "maker code has to be two characters",
    )?);
    out.u8(boot.disc_number);
    out.u8(boot.revision);
    out.u8(boot.audio_streaming);
    out.u8(boot.stream_buffer_size);

    pad_to(out, MAGIC_OFFSET);
    out.u32(MAGIC);

    // Short titles keep their terminator, and one that fills the field has
    // none, which is how the reader takes it back.
    pad_to(out, TITLE_OFFSET);
    let title = encode(&boot.title, "title is not Shift-JIS")?;
    if title.len() > TITLE_LEN {
        return Err(Error::Unwritable("title does not fit its 64 byte field"));
    }
    out.bytes(&title);

    pad_to(out, AUTHORED_LEN);
    Ok(())
}

/// Writes the disc metadata back out: six fields in eight kilobytes of nothing.
pub fn bi2_bin(bi2: &Bi2) -> Vec<u8> {
    let mut out = Writer::with_capacity(BI2_LEN as usize);

    pad_to(&mut out, BI2_SIMULATED_MEMORY_SIZE);
    out.u32(bi2.simulated_memory_size);
    pad_to(&mut out, BI2_DEBUG_FLAG);
    out.u32(bi2.debug_flag);
    pad_to(&mut out, BI2_COUNTRY);
    out.u32(bi2.country);
    out.u32(bi2.unknown_1c);
    out.u32(bi2.unknown_20);
    out.u32(bi2.pad_spec);

    pad_to(&mut out, BI2_LEN as usize);
    out.finish()
}

/// Zero fills up to the next field, which is how both files above cross the
/// stretches that hold nothing.
fn pad_to(out: &mut Writer, offset: usize) {
    out.zeros(offset - out.len());
}

/// Encodes a text field back to the Shift-JIS the reader took it out of, and
/// requires it to still be the width of its field.
fn exactly(text: &str, len: usize, what: &'static str) -> Result<Vec<u8>> {
    let bytes = encode(text, what)?;
    match bytes.len() == len {
        true => Ok(bytes),
        false => Err(Error::Unwritable(what)),
    }
}

fn encode(text: &str, what: &'static str) -> Result<Vec<u8>> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(text);
    match unmappable {
        true => Err(Error::Unwritable(what)),
        false => Ok(bytes.into_owned()),
    }
}

/// Decodes one of the header's text fields. Shift-JIS, following the file table
/// and the archives, which is the same as ASCII for every print of this game.
fn text(raw: &[u8], what: &'static str) -> Result<String> {
    let (text, _, malformed) = encoding_rs::SHIFT_JIS.decode(raw);
    match malformed {
        true => Err(Error::CorruptHeader(what)),
        false => Ok(text.into_owned()),
    }
}

/// Where the file table sits, out of the boot header.
pub(crate) fn fst_range(boot: &[u8]) -> Result<(u64, u64)> {
    let reader = Reader::new(boot);
    Ok((
        reader.u32_at(FST_OFFSET_FIELD)? as u64,
        reader.u32_at(FST_SIZE_FIELD)? as u64,
    ))
}

/// The apploader states its own length in two parts, neither of which counts
/// its header.
pub(crate) fn apploader_len(header: &[u8]) -> Result<u64> {
    let reader = Reader::new(header);
    let size = reader.u32_at(APPLOADER_SIZE_FIELD)? as u64;
    let trailer = reader.u32_at(APPLOADER_TRAILER_FIELD)? as u64;
    Ok(APPLOADER_HEADER_LEN + size + trailer)
}

/// The two preamble pieces a project keeps as files, neither of which records
/// its length anywhere but inside itself.
///
/// The other three are not files. The boot header and the disc metadata are a
/// few values each, kept as `Metadata`. `fst` derives the file table.
pub(crate) fn entries(disc: &Disc) -> Result<Vec<Entry>> {
    let boot = disc.read(0, BOOT_LEN)?;
    let dol_offset = Reader::new(&boot).u32_at(DOL_OFFSET_FIELD)? as u64;
    let (fst_offset, _) = fst_range(&boot)?;

    // A game disc with nowhere to boot from is a header that did not survive
    // whatever produced it.
    if dol_offset == 0 {
        return Err(Error::CorruptHeader("there is no executable"));
    }

    // Nothing records the executable's length, so it is the one a corrupt header
    // can inflate without contradicting itself.
    let dol_len = dol_len(disc, dol_offset)?;
    if dol_offset < fst_offset && dol_offset + dol_len > fst_offset {
        return Err(Error::CorruptHeader(
            "the executable runs into the file table",
        ));
    }

    let apploader = disc.read(APPLOADER_OFFSET, APPLOADER_HEADER_LEN)?;
    let entry = |path: &str, offset, size| Entry::File {
        path: path.to_string(),
        offset,
        size,
    };

    Ok(vec![
        entry(APPLOADER_PATH, APPLOADER_OFFSET, apploader_len(&apploader)?),
        entry(DOL_PATH, dol_offset, dol_len),
    ])
}

/// An executable is as long as its furthest section reaches. Its 18 section
/// offsets and 18 lengths sit in two runs in the header, in step with each
/// other, so a section that is not present reads as zero and reaches nowhere.
fn dol_len(disc: &Disc, offset: u64) -> Result<u64> {
    let header = disc.read(offset, DOL_HEADER_LEN)?;
    let reader = Reader::new(&header);

    let mut end = DOL_HEADER_LEN;
    for section in 0..DOL_SECTIONS {
        let at = section * 4;
        let start = reader.u32_at(DOL_SECTION_OFFSETS + at)? as u64;
        let len = reader.u32_at(DOL_SECTION_SIZES + at)? as u64;
        end = end.max(start + len);
    }
    Ok(end)
}
