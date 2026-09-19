//! The read path: turns a message file's bytes into a [`Bmg`], nothing
//! copied out of the input.

use tpmt_bytes::Reader;

use crate::sections::{self, flow, message};
use crate::{Bmg, Encoding, Error, Format, Result, header};

/// The sections a file holds, sorted out by name on the way past.
/// INF1 and DAT1 are always there, so they are required fields rather than
/// `Option`s left to be checked after the fact.
struct Sections<'a> {
    inf1: &'a [u8],
    dat1: &'a [u8],
    mid1: Option<&'a [u8]>,
    str1: Option<&'a [u8]>,
    /// FLW1 and FLI1 together, which are always both present or both absent.
    /// Paired up here rather than kept as two fields so that state is
    /// unrepresentable instead of checked for.
    flow: Option<(&'a [u8], &'a [u8])>,
}

/// Walks the section table, sorting every section out by name.
///
/// Each section says how far the next one is, so the table is walked from the
/// front. A count larger than the file holds runs out of bytes on the section
/// it invents, rather than being trusted as an allocation.
// `flw1`/`fli1` mirror the file format's own FLW1/FLI1 section names.
#[allow(clippy::similar_names)]
fn split(data: &[u8]) -> Result<(Encoding, Sections<'_>)> {
    let reader = Reader::new(data);
    let encoding = Encoding::from_byte(reader.u8_at(header::ENCODING)?);
    let count = reader.u32_at(header::SECTION_COUNT)? as usize;

    let mut at = header::LEN;
    // What the header should have stated: the flow sections are left out of
    // it, so they are left out of this too.
    let mut stated = header::LEN;
    let mut inf1 = None;
    let mut dat1 = None;
    let mut mid1 = None;
    let mut str1 = None;
    let mut flw1 = None;
    let mut fli1 = None;

    for _ in 0..count {
        let magic: [u8; 4] = reader.bytes_at(at)?;
        let size = reader.u32_at(at + sections::SIZE)? as usize;
        if size < sections::HEADER_LEN {
            return Err(Error::Corrupt("a section is smaller than its own header"));
        }

        // The last section in a file is allowed to stop where the file does,
        // with the padding its stated size counts left off the end.
        let body_at = at + sections::HEADER_LEN;
        let len = (size - sections::HEADER_LEN).min(data.len() - body_at);
        let body = reader.slice_at(body_at, len)?;

        match magic {
            sections::INF1 => inf1 = Some(body),
            sections::DAT1 => dat1 = Some(body),
            sections::MID1 => mid1 = Some(body),
            sections::STR1 => str1 = Some(body),
            sections::FLW1 => flw1 = Some(body),
            sections::FLI1 => fli1 = Some(body),
            _ => return Err(Error::Corrupt("a section has a name no file has")),
        }
        at += size;
        if !matches!(magic, sections::FLW1 | sections::FLI1) {
            stated += size;
        }
    }

    // The one number in the file that says anything about the rest of it, so
    // it is checked rather than skipped: a walk that ends somewhere else read
    // a size wrong, or the file is not laid out the way it claims.
    if reader.u32_at(header::SIZE)? as usize != stated {
        return Err(Error::Corrupt(
            "the stated size is not where the sections end",
        ));
    }

    let flow = match (flw1, fli1) {
        (Some(flw1), Some(fli1)) => Some((flw1, fli1)),
        (None, None) => None,
        _ => {
            return Err(Error::Corrupt(
                "the flow graph is missing one of its two sections",
            ));
        }
    };

    Ok((
        encoding,
        Sections {
            inf1: inf1.ok_or(Error::Corrupt("there is no message table"))?,
            dat1: dat1.ok_or(Error::Corrupt("there is no message text"))?,
            mid1,
            str1,
            flow,
        },
    ))
}

pub fn unpack(data: &[u8]) -> Result<Bmg> {
    if !Bmg::recognises(data) {
        return Err(Error::NotBmg);
    }

    let (encoding, sections) = split(data)?;

    let (messages, record_len, mid1) = message::read(sections.inf1, sections.dat1, sections.mid1)?;
    Ok(Bmg {
        encoding,
        record_len,
        mid1,
        messages,
        flow: sections
            .flow
            .map(|(flw1, fli1)| flow::read(flw1, fli1))
            .transpose()?,
        strings: sections.str1.map(read_strings),
    })
}

/// The string pool split on its terminators.
///
/// Splitting and rejoining are the same operation backwards, so trailing empty
/// entries are kept rather than trimmed: they are how a pool that ends in
/// several terminators comes back as the bytes it was.
fn read_strings(str1: &[u8]) -> Vec<Vec<u8>> {
    str1.split(|&byte| byte == 0).map(<[u8]>::to_vec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of `magics`, one 0x10 section each, stating `size` for them.
    fn file(size: u32, magics: &[&[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(header::MAGIC);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&u32::try_from(magics.len()).unwrap().to_be_bytes());
        out.push(Encoding::ShiftJis.byte());
        out.resize(header::LEN, 0);
        for magic in magics {
            out.extend_from_slice(*magic);
            out.extend_from_slice(&0x10u32.to_be_bytes());
            out.resize(out.len() + 8, 0);
        }
        out
    }

    /// The stated size covers the header and every section but the flow pair,
    /// which is what makes it worth checking: a file stating its whole length
    /// instead is one whose sections are not where it says they are.
    ///
    /// Only the walk is exercised, since taking the sections apart is still todo
    #[test]
    fn the_stated_size_leaves_the_flow_sections_out() {
        let magics = [b"INF1", b"DAT1", b"FLW1", b"FLI1"];
        let data = file(0x40, &magics);
        let (encoding, sections) = split(&data).unwrap();
        assert_eq!(encoding, Encoding::ShiftJis);
        assert!(sections.flow.is_some());

        // 0x60 is the whole file, flow sections and all.
        let whole = file(0x60, &magics);
        assert!(matches!(split(&whole), Err(Error::Corrupt(_))));
    }

    #[test]
    fn an_unknown_section_is_refused() {
        let data = file(0x50, &[b"INF1", b"DAT1", b"XXXX"]);
        assert!(matches!(split(&data), Err(Error::Corrupt(_))));
    }

    #[test]
    fn the_string_pool_keeps_its_empty_entries() {
        assert_eq!(
            read_strings(b"\0arrow\0arrows\0"),
            [b"".as_slice(), b"arrow", b"arrows", b""]
        );
    }
}
