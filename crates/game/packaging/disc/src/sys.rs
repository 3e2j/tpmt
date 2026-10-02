//! The preamble: the fixed run at the front of a disc that says what everything
//! else is and where it went.
//!
//! Boot header, disc metadata, apploader, executable. Only the first two sit
//! where the format fixes them. The boot header says where the other two are,
//! and neither of those records its own length anywhere but inside itself.
//!
//! The first two are almost entirely empty, and most of what they do hold is a
//! consequence of the layout rather than anything anyone chose. What is left is
//! thirteen values, so they come back as `Metadata` instead of as files.
//! Reading either one builds it again from those values and compares, so a
//! disc that does not match is refused rather than quietly rebuilt into
//! something else.

use serde::{Deserialize, Serialize};
use tpmt_bytes::{Be32, Layout, Reader};

use crate::{Disc, Entry, Error, Result, Span};

// Boot header. The magic is what makes this a GameCube disc rather than
// anything else that happens to be 1.4 GB.
pub const MAGIC: u32 = 0xC233_9F3D;
pub const WII_MAGIC: u32 = 0x5D1C_9EA3;
pub const ID_LEN: usize = 4;
pub const MAKER_LEN: usize = 2;
pub const TITLE_LEN: usize = 0x40;

tpmt_bytes::layout! {
    /// The front of the boot header, where every value a project keeps sits.
    #[derive(Clone, Copy)]
    pub struct Authored {
        pub id: [u8; ID_LEN],
        pub maker: [u8; MAKER_LEN],
        pub disc_number: u8,
        pub revision: u8,
        pub audio_streaming: u8,
        pub stream_buffer_size: u8,
        pub unnamed_0a: [u8; 0x0E],
        /// [`WII_MAGIC`] on a Wii disc. Zero on this one.
        pub wii_magic: Be32,
        pub magic: Be32,
        /// Only terminated when the title is short enough to leave room.
        pub title: [u8; TITLE_LEN],
    }
}

tpmt_bytes::layout! {
    /// The boot header. Past [`Authored`] it is all layout, worked out again
    /// by a build rather than kept. `DVDBB2` in the SDK covers the seven
    /// fields from `dol_offset` on.
    #[derive(Clone, Copy)]
    pub struct BootBin {
        pub authored: Authored,
        pub unnamed_60: [u8; 0x3A0],
        /// The mastering put the apploader's length here, whatever it meant
        /// by it, and nothing on a retail disc reads it.
        pub debug_monitor: Be32,
        pub debug_monitor_address: Be32,
        pub unnamed_408: [u8; 0x18],
        pub dol_offset: Be32,
        pub fst_offset: Be32,
        pub fst_size: Be32,
        /// Only ever different from `fst_size` on a game spanning several
        /// discs.
        pub fst_max_size: Be32,
        pub fst_address: Be32,
        pub user_position: Be32,
        pub user_length: Be32,
        pub unnamed_43c: [u8; 4],
    }
}

/// Where the debug monitor would be loaded. Nothing on a retail disc reads it.
pub const DEBUG_MONITOR_ADDRESS: u32 = 0x8028_0060;
/// The file table is loaded as high as it fits under here, and the arena ends
/// where it starts.
const FST_TOP: u32 = 0x8040_0000;
/// The end of a `GameCube` disc's user area.
pub const USER_AREA_END: u32 = 0x5705_8000;
/// User data starts on one of these, past the file table.
const USER_ALIGN: u32 = 0x8000;
/// The executable and the file table each start on one of these, past whatever
/// the layout put in front of them.
pub const PREAMBLE_ALIGN: u64 = 0x100;

// Disc metadata, then the apploader, at fixed positions after the boot header.
pub const BI2_OFFSET: u64 = 0x440;
pub const APPLOADER_OFFSET: u64 = 0x2440;

tpmt_bytes::layout! {
    /// The disc metadata: six fields and then eight kilobytes of nothing.
    pub struct Bi2Bin {
        pub debug_monitor_size: Be32,
        pub simulated_memory_size: Be32,
        pub argument_offset: Be32,
        pub debug_flag: Be32,
        pub track_location: Be32,
        pub track_size: Be32,
        pub country: Be32,
        pub unknown_1c: Be32,
        pub unknown_20: Be32,
        /// `__PADSpec`, which `OSInit` reads straight out of here.
        pub pad_spec: Be32,
        pub unnamed_28: [u8; 0x1FD8],
    }
}

tpmt_bytes::layout! {
    /// What the apploader opens with. It states its own length in two parts,
    /// neither of which counts this header.
    pub struct ApploaderHeader {
        /// The build date, as text.
        pub date: [u8; 0x10],
        pub entry_point: Be32,
        pub size: Be32,
        pub trailer_size: Be32,
        pub unnamed: [u8; 4],
    }
}

pub const BOOT: Span = Span {
    offset: 0,
    size: BootBin::LEN as u64,
};
pub const BI2: Span = Span {
    offset: BI2_OFFSET,
    size: Bi2Bin::LEN as u64,
};
pub const APPLOADER_HEADER: Span = Span {
    offset: APPLOADER_OFFSET,
    size: ApploaderHeader::LEN as u64,
};

/// Where the two preamble files land in a project. A build looks for them by
/// these names, so they are spelled once.
pub const APPLOADER_PATH: &str = "sys/apploader.img";
pub const DOL_PATH: &str = "sys/main.dol";
/// The two preamble pieces, which are not files a project holds: they are kept
/// as their values and built again. Named here because a mod that carries one
/// has to call it what a disc calls it.
pub const BOOT_PATH: &str = "sys/boot.bin";
pub const BI2_PATH: &str = "sys/bi2.bin";

// Executable. Its length is not stored anywhere, so it is whatever the furthest
// section reaches.
pub const DOL_SECTIONS: usize = 18;

tpmt_bytes::layout! {
    /// The executable's header. Its section offsets, load addresses and
    /// lengths are three runs in step with each other, so a section that is
    /// not present reads as zero in all three.
    pub struct DolHeader {
        pub section_offsets: [Be32; DOL_SECTIONS],
        pub section_addresses: [Be32; DOL_SECTIONS],
        pub section_sizes: [Be32; DOL_SECTIONS],
        pub bss_address: Be32,
        pub bss_size: Be32,
        pub entry_point: Be32,
        pub unnamed: [u8; 0x1C],
    }
}

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
    /// the file table. The library that would act on it is not linked into
    /// the game.
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

/// Refuses anything that is not a `GameCube` disc image.
///
/// Called before the rest of the preamble is read, so a file that is not a disc
/// says so rather than failing on a short read somewhere inside it.
pub fn identify(boot: &[u8]) -> Result<()> {
    let authored: &Authored = Reader::new(boot).view_at(0)?;
    if authored.magic.get() == MAGIC {
        return Ok(());
    }

    // Both magics sit in the same header and only one is ever set, so a Wii
    // disc can be declined by name rather than as a mystery.
    if authored.wii_magic.get() == WII_MAGIC {
        Err(Error::WiiDisc)
    } else {
        Err(Error::NotADisc)
    }
}

/// Reads the boot header, having already been told it is one by `identify`.
///
/// The executable's and file table's offsets are taken as read, since a build
/// picks fresh ones anyway.
pub fn boot(bytes: &[u8], apploader_len: u64) -> Result<Boot> {
    let header: &BootBin = Reader::new(bytes).view_at(0)?;
    let authored = &header.authored;

    // A 64 byte field, only terminated when the title is short enough to leave
    // room, so the read stops at the end of the field either way.
    let title = &authored.title;
    let title = &title[..title.iter().position(|&b| b == 0).unwrap_or(title.len())];

    let boot = Boot {
        id: text(&authored.id, "the game id is not text")?,
        maker: text(&authored.maker, "the maker code is not text")?,
        disc_number: authored.disc_number,
        revision: authored.revision,
        audio_streaming: authored.audio_streaming,
        stream_buffer_size: authored.stream_buffer_size,
        title: text(title, "the title is not text")?,
    };

    let apploader_len = u32::try_from(apploader_len)
        .map_err(|_| Error::CorruptHeader("the apploader is too long for its own header"))?;
    let layout = BootLayout {
        apploader_len,
        dol_offset: header.dol_offset.get(),
        fst_offset: header.fst_offset.get(),
        fst_len: header.fst_size.get(),
    };
    unchanged(
        header.as_bytes(),
        boot_bin(&boot, &layout)?.as_bytes(),
        "the boot header",
        0,
    )?;
    Ok(boot)
}

/// The file table is loaded as high as it goes, on a 32 byte boundary because
/// `DVDChangeDisk` asserts on that.
const fn fst_address(fst_len: u32) -> u32 {
    FST_TOP.saturating_sub(fst_len) & !31
}

/// User data starts on the first boundary past the file table.
pub fn user_position(fst_offset: u32, fst_len: u32) -> u32 {
    fst_offset
        .saturating_add(fst_len)
        .checked_next_multiple_of(USER_ALIGN)
        .unwrap_or(0)
}

/// Reads the disc metadata.
pub fn bi2(bytes: &[u8]) -> Result<Bi2> {
    let header: &Bi2Bin = Reader::new(bytes).view_at(0)?;
    let bi2 = Bi2 {
        simulated_memory_size: header.simulated_memory_size.get(),
        debug_flag: header.debug_flag.get(),
        country: header.country.get(),
        unknown_1c: header.unknown_1c.get(),
        unknown_20: header.unknown_20.get(),
        pad_spec: header.pad_spec.get(),
    };
    unchanged(
        header.as_bytes(),
        bi2_bin(&bi2).as_bytes(),
        "the disc metadata",
        BI2_OFFSET,
    )?;
    Ok(bi2)
}

/// Refuses a header whose rebuild differs from what was read, naming the first
/// byte that would change as a position on the disc.
fn unchanged(read: &[u8], rebuilt: &[u8], region: &'static str, at: u64) -> Result<()> {
    read.iter()
        .zip(rebuilt)
        .position(|(a, b)| a != b)
        .map_or(Ok(()), |offset| {
            Err(Error::PreambleWouldChange {
                region,
                offset: at + offset as u64,
            })
        })
}

/// The four positions a layout works out, which the boot header restates.
pub struct BootLayout {
    pub(crate) apploader_len: u32,
    pub(crate) dol_offset: u32,
    pub(crate) fst_offset: u32,
    pub(crate) fst_len: u32,
}

/// Writes the boot header back out: the inverse of `boot`.
///
/// Seven kept values and the magic. Everything else is a run of zeros, or a
/// number that follows from where the layout put the three things the header
/// points at.
pub fn boot_bin(boot: &Boot, layout: &BootLayout) -> Result<BootBin> {
    let &BootLayout {
        apploader_len,
        dol_offset,
        fst_offset,
        fst_len,
    } = layout;

    let user = user_position(fst_offset, fst_len);
    Ok(BootBin {
        authored: authored(boot)?,
        unnamed_60: [0; 0x3A0],
        debug_monitor: Be32::new(apploader_len),
        debug_monitor_address: Be32::new(DEBUG_MONITOR_ADDRESS),
        unnamed_408: [0; 0x18],
        dol_offset: Be32::new(dol_offset),
        fst_offset: Be32::new(fst_offset),
        fst_size: Be32::new(fst_len),
        // Only ever larger on a game spanning several discs, and this is one
        // disc.
        fst_max_size: Be32::new(fst_len),
        fst_address: Be32::new(fst_address(fst_len)),
        user_position: Be32::new(user),
        user_length: Be32::new(USER_AREA_END.saturating_sub(user)),
        unnamed_43c: [0; 4],
    })
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
///
/// # Errors
///
/// - [`Error::Unwritable`] if `original` is not `0x440` bytes, or an edited
///   field doesn't fit its slot (a game id or maker code of the wrong
///   length, a title that isn't Shift-JIS, or one that overruns its 64 byte
///   field).
pub fn boot_bin_over(original: &[u8], boot: &Boot) -> Result<BootBin> {
    if original.len() != BootBin::LEN {
        return Err(Error::Unwritable("a boot header is 0x440 bytes"));
    }

    let original: &BootBin = Reader::new(original).view_at(0)?;
    Ok(BootBin {
        authored: authored(boot)?,
        ..*original
    })
}

/// The part of the header a project keeps: seven values and the magic, and the
/// zeros between them.
fn authored(boot: &Boot) -> Result<Authored> {
    let id = exactly(&boot.id, "game id has to be four characters")?;
    let maker = exactly(&boot.maker, "maker code has to be two characters")?;

    // Short titles keep their terminator, and one that fills the field has
    // none, which is how the reader takes it back.
    let encoded = encode(&boot.title, "title is not Shift-JIS")?;
    let mut title = [0; TITLE_LEN];
    title
        .get_mut(..encoded.len())
        .ok_or(Error::Unwritable("title does not fit its 64 byte field"))?
        .copy_from_slice(&encoded);

    Ok(Authored {
        id,
        maker,
        disc_number: boot.disc_number,
        revision: boot.revision,
        audio_streaming: boot.audio_streaming,
        stream_buffer_size: boot.stream_buffer_size,
        unnamed_0a: [0; 0x0E],
        wii_magic: Be32::new(0),
        magic: Be32::new(MAGIC),
        title,
    })
}

/// Writes the disc metadata back out: six fields in eight kilobytes of nothing.
#[must_use]
pub const fn bi2_bin(bi2: &Bi2) -> Bi2Bin {
    Bi2Bin {
        debug_monitor_size: Be32::new(0),
        simulated_memory_size: Be32::new(bi2.simulated_memory_size),
        argument_offset: Be32::new(0),
        debug_flag: Be32::new(bi2.debug_flag),
        track_location: Be32::new(0),
        track_size: Be32::new(0),
        country: Be32::new(bi2.country),
        unknown_1c: Be32::new(bi2.unknown_1c),
        unknown_20: Be32::new(bi2.unknown_20),
        pad_spec: Be32::new(bi2.pad_spec),
        unnamed_28: [0; 0x1FD8],
    }
}

/// Encodes a text field back to the Shift-JIS the reader took it out of, and
/// requires it to still be the width of its field.
fn exactly<const N: usize>(text: &str, what: &'static str) -> Result<[u8; N]> {
    encode(text, what)?
        .try_into()
        .map_err(|_| Error::Unwritable(what))
}

fn encode(text: &str, what: &'static str) -> Result<Vec<u8>> {
    let (bytes, _, unmappable) = encoding_rs::SHIFT_JIS.encode(text);
    if unmappable {
        Err(Error::Unwritable(what))
    } else {
        Ok(bytes.into_owned())
    }
}

/// Decodes one of the header's text fields. Shift-JIS, following the file
/// table and the archives.
fn text(raw: &[u8], what: &'static str) -> Result<String> {
    let (text, _, malformed) = encoding_rs::SHIFT_JIS.decode(raw);
    if malformed {
        Err(Error::CorruptHeader(what))
    } else {
        Ok(text.into_owned())
    }
}

/// Where the file table sits, out of the boot header.
pub fn fst_range(boot: &[u8]) -> Result<Span> {
    let header: &BootBin = Reader::new(boot).view_at(0)?;
    Ok(Span {
        offset: header.fst_offset.get() as u64,
        size: header.fst_size.get() as u64,
    })
}

/// The apploader states its own length in two parts, neither of which counts
/// its header.
pub fn apploader_len(header: &[u8]) -> Result<u64> {
    let header: &ApploaderHeader = Reader::new(header).view_at(0)?;
    Ok(ApploaderHeader::LEN as u64 + header.size.get() as u64 + header.trailer_size.get() as u64)
}

/// The two preamble pieces a project keeps as files, neither of which records
/// its length anywhere but inside itself.
///
/// The other three are not files. The boot header and the disc metadata are a
/// few values each, kept as `Metadata`. `fst` derives the file table.
pub fn entries(disc: &Disc) -> Result<Vec<Entry>> {
    let boot = disc.read(BOOT)?;
    let dol_offset = Reader::new(&boot).view_at::<BootBin>(0)?.dol_offset.get() as u64;
    let fst_offset = fst_range(&boot)?.offset;

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

    let apploader = disc.read(APPLOADER_HEADER)?;
    let entry = |path: &str, offset, size| Entry::File {
        path: path.to_string(),
        span: Span { offset, size },
    };

    Ok(vec![
        entry(APPLOADER_PATH, APPLOADER_OFFSET, apploader_len(&apploader)?),
        entry(DOL_PATH, dol_offset, dol_len),
    ])
}

/// An executable is as long as its furthest section reaches.
fn dol_len(disc: &Disc, offset: u64) -> Result<u64> {
    let header = disc.read(Span {
        offset,
        size: DolHeader::LEN as u64,
    })?;
    let header: &DolHeader = Reader::new(&header).view_at(0)?;

    Ok(header
        .section_offsets
        .iter()
        .zip(&header.section_sizes)
        .map(|(start, len)| start.get() as u64 + len.get() as u64)
        .fold(DolHeader::LEN as u64, u64::max))
}
