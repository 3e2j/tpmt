//! The preamble: the fixed run at the front of a disc that says what everything
//! else is and where it went.
//!
//! Boot header, disc metadata, apploader, executable. Only the first two sit
//! where the format fixes them. The boot header says where the other two are,
//! and neither of those records its own length anywhere but inside itself.

use tpmt_bytes::Reader;

use crate::{Disc, Entry, Error, Result};

// Boot header. The magic is what makes this a GameCube disc rather than
// anything else that happens to be 1.4 GB.
pub(crate) const MAGIC: u32 = 0xC233_9F3D;
pub(crate) const MAGIC_OFFSET: usize = 0x1C;
pub(crate) const WII_MAGIC: u32 = 0x5D1C_9EA3;
pub(crate) const WII_MAGIC_OFFSET: usize = 0x18;
pub(crate) const ID_LEN: usize = 4;
pub(crate) const REVISION_OFFSET: usize = 0x07;
pub(crate) const TITLE_OFFSET: usize = 0x20;
pub(crate) const TITLE_LEN: usize = 0x40;
pub(crate) const BOOT_LEN: u64 = 0x440;

pub(crate) const DOL_OFFSET_FIELD: usize = 0x420;
pub(crate) const FST_OFFSET_FIELD: usize = 0x424;
pub(crate) const FST_SIZE_FIELD: usize = 0x428;

// Disc metadata, then the apploader, at fixed positions after the boot header.
pub(crate) const BI2_OFFSET: u64 = 0x440;
pub(crate) const BI2_LEN: u64 = 0x2000;
pub(crate) const APPLOADER_OFFSET: u64 = 0x2440;
pub(crate) const APPLOADER_HEADER_LEN: u64 = 0x20;
pub(crate) const APPLOADER_SIZE_FIELD: usize = 0x14;
pub(crate) const APPLOADER_TRAILER_FIELD: usize = 0x18;

// Executable. Its length is not stored anywhere, so it is whatever the furthest
// section reaches.
pub(crate) const DOL_HEADER_LEN: u64 = 0x100;
pub(crate) const DOL_SECTIONS: usize = 18;
pub(crate) const DOL_SECTION_OFFSETS: usize = 0x00;
pub(crate) const DOL_SECTION_SIZES: usize = 0x90;

/// What a disc calls itself, out of the boot header.
#[derive(Debug, Clone)]
pub struct Header {
    /// Four characters: system, game code, region. `GZ2E` and friends. The two
    /// maker-code bytes after it are dropped, being the same on every disc this
    /// toolkit will ever see.
    pub id: String,
    /// Revision of the print, `0` for the first.
    pub revision: u8,
    pub title: String,
}

/// Reads the boot header, which is where a disc is also checked for being one.
pub(crate) fn header(boot: &[u8]) -> Result<Header> {
    let reader = Reader::new(boot);
    if reader.u32_at(MAGIC_OFFSET)? != MAGIC {
        // Both magics sit in the same header and only one is ever set, so a Wii
        // disc can be declined by name rather than as a mystery.
        return match reader.u32_at(WII_MAGIC_OFFSET)? == WII_MAGIC {
            true => Err(Error::WiiDisc),
            false => Err(Error::NotADisc),
        };
    }

    // A 64 byte field, only terminated when the title is short enough to leave
    // room, so the read stops at the end of the field either way.
    let title = reader.slice_at(TITLE_OFFSET, TITLE_LEN)?;
    let title = &title[..title.iter().position(|&b| b == 0).unwrap_or(title.len())];

    Ok(Header {
        id: String::from_utf8_lossy(reader.slice_at(0, ID_LEN)?).into_owned(),
        revision: reader.u8_at(REVISION_OFFSET)?,
        title: String::from_utf8_lossy(title).into_owned(),
    })
}

/// Where the file table sits, out of the boot header.
pub(crate) fn fst_range(boot: &[u8]) -> Result<(u64, u64)> {
    let reader = Reader::new(boot);
    Ok((
        reader.u32_at(FST_OFFSET_FIELD)? as u64,
        reader.u32_at(FST_SIZE_FIELD)? as u64,
    ))
}

/// The five preamble pieces. Only the boot header says where any of them are,
/// and two have to have their lengths worked out from their own contents.
pub(crate) fn entries(disc: &Disc) -> Result<Vec<Entry>> {
    let boot = disc.read(0, BOOT_LEN)?;
    let dol_offset = Reader::new(&boot).u32_at(DOL_OFFSET_FIELD)? as u64;
    let (fst_offset, fst_size) = fst_range(&boot)?;

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

    let entry = |path: &str, offset, size| Entry::File {
        path: format!("sys/{path}"),
        offset,
        size,
    };

    Ok(vec![
        entry("boot.bin", 0, BOOT_LEN),
        entry("bi2.bin", BI2_OFFSET, BI2_LEN),
        entry(
            "apploader.img",
            APPLOADER_OFFSET,
            apploader_len(disc, APPLOADER_OFFSET)?,
        ),
        entry("main.dol", dol_offset, dol_len),
        entry("fst.bin", fst_offset, fst_size),
    ])
}

/// The apploader states its own length in two parts, neither of which counts its
/// header.
fn apploader_len(disc: &Disc, offset: u64) -> Result<u64> {
    let header = disc.read(offset, APPLOADER_HEADER_LEN)?;
    let reader = Reader::new(&header);
    let size = reader.u32_at(APPLOADER_SIZE_FIELD)? as u64;
    let trailer = reader.u32_at(APPLOADER_TRAILER_FIELD)? as u64;
    Ok(APPLOADER_HEADER_LEN + size + trailer)
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
