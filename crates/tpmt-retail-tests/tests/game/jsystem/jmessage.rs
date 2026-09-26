//! Message files, against `tpmt_game::jsystem::jmessage`.

use tpmt_format::Format;
use tpmt_game::jsystem::jmessage::{self, Field, Layout, record, tag, unit};
use tpmt_game::{Edition, Version};
use tpmt_jmessage::{Bmg, Message, TEXT_OFFSET_LEN, TextSegment};

/// The record width picks the layout the game reads the file with:
/// [`unit::layout`] for `zel_unit.bmg`, [`record::LAYOUT`] for every other
/// file.
pub fn layout(version: Version, path: &str, bytes: &[u8]) -> Vec<String> {
    let (edition, bmg) = match decode(version, bytes) {
        Ok(decoded) => decoded,
        Err(problem) => return vec![problem],
    };
    let expected = expected(edition, path);
    match jmessage::layout(edition, bmg.record_len) {
        Some(layout) if layout == expected => Vec::new(),
        Some(layout) => vec![format!(
            "{} byte records read as the {} byte layout, not the {} byte one",
            bmg.record_len, layout.len, expected.len
        )],
        None => vec![format!("no layout is {} bytes wide", bmg.record_len)],
    }
}

/// In a file with MID1, each record's id field is its message's MID1 id.
pub fn id(version: Version, path: &str, bytes: &[u8]) -> Vec<String> {
    let (edition, bmg) = match decode(version, bytes) {
        Ok(decoded) => decoded,
        Err(problem) => return vec![problem],
    };
    let Some(id) = read_layout(edition, path, &bmg).and_then(|layout| layout.id) else {
        return Vec::new();
    };
    if bmg.mid1.is_none() {
        return Vec::new();
    }
    bmg.messages
        .iter()
        .filter_map(|message| {
            let value = value(id, message);
            (value != Some(message.public_id)).then(|| {
                format!(
                    "record {}: {} is {value:?}, MID1 is {}",
                    message.id.0, id.name, message.public_id
                )
            })
        })
        .collect()
}

/// Every padding field is 0.
pub fn padding(version: Version, path: &str, bytes: &[u8]) -> Vec<String> {
    let (edition, bmg) = match decode(version, bytes) {
        Ok(decoded) => decoded,
        Err(problem) => return vec![problem],
    };
    let Some(layout) = read_layout(edition, path, &bmg) else {
        return Vec::new();
    };
    let padding = layout.fields.iter().filter(|field| field.name == "Padding");
    padding
        .flat_map(|field| bmg.messages.iter().map(move |message| (field, message)))
        .filter_map(|(field, message)| {
            let value = value(*field, message);
            (value != Some(0)).then(|| {
                format!(
                    "record {}: padding at {:#x} is {value:?}",
                    message.id.0, field.offset
                )
            })
        })
        .collect()
}

/// Each tag the table names carries the argument bytes its [`tag::Args`]
/// gives.
pub fn tags(version: Version, _: &str, bytes: &[u8]) -> Vec<String> {
    let (edition, bmg) = match decode(version, bytes) {
        Ok(decoded) => decoded,
        Err(problem) => return vec![problem],
    };
    let mut problems = Vec::new();
    for message in &bmg.messages {
        problems.extend(message_tags(edition, message));
    }
    problems
}

// Helpers

/// The file, and the edition to read it as.
fn decode(version: Version, bytes: &[u8]) -> Result<(Edition, Bmg), String> {
    let bmg = Bmg::decode(bytes).map_err(|error| format!("decode failed: {error}"))?;
    // Layouts and tags split by version only, so any language will do.
    Ok((Edition::default_language(version), bmg))
}

fn expected(edition: Edition, path: &str) -> &'static Layout {
    if path.ends_with("/zel_unit.bmg") {
        unit::layout(edition)
    } else {
        &record::LAYOUT
    }
}

/// The layout `bmg`'s records are in, or `None` when the width picks some
/// other layout than the one the game reads the file with. [`layout`] reports
/// that, so the checks that read fields skip the file.
fn read_layout(edition: Edition, path: &str, bmg: &Bmg) -> Option<&'static Layout> {
    jmessage::layout(edition, bmg.record_len).filter(|&layout| layout == expected(edition, path))
}

/// `field`'s value in `message`, or `None` when the record is too short to
/// hold it.
fn value(field: Field, message: &Message) -> Option<u16> {
    let start = field.offset.checked_sub(usize::from(TEXT_OFFSET_LEN))?;
    match message.attributes.get(start..start + field.len)? {
        [byte] => Some(u16::from(*byte)),
        [high, low] => Some(u16::from_be_bytes([*high, *low])),
        _ => None,
    }
}

fn message_tags(edition: Edition, message: &Message) -> Vec<String> {
    message
        .text
        .iter()
        .filter_map(|segment| match segment {
            TextSegment::Tag { group, code, args } => Some((*group, *code, args)),
            TextSegment::Text(_) => None,
        })
        .filter_map(|(group, code, args)| {
            let tag = tag::find(group, code, edition)?;
            // A varying length is a u8 then the rest, so at least 1.
            let fits = tag
                .args
                .fixed_len()
                .map_or(!args.is_empty(), |len| args.len() == len);
            (!fits).then(|| {
                format!(
                    "record {}: {} ({group}, {code}) has {} argument bytes, the table says {:?}",
                    message.id.0,
                    tag.name,
                    args.len(),
                    tag.args
                )
            })
        })
        .collect()
}
