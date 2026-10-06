//! Message text as a readable string, with tags named from the tables.
//!
//! Text runs are decoded in the file's own encoding. Each tag becomes a
//! token in braces, named from `tpmt_tables::message::tag` where it can be:
//!
//! ```text
//! {Player name}           a tag with no arguments
//! {Pause:30}              a number argument
//! {Color:Red}             a 1-byte argument named from the tag's value table
//! {Sound:20}              a sound or camera tag, whose code is its value
//! {Ruby:2:reading}        ruby: the base character count, then the reading
//! {#5.3:01}               any other tag, as group.code and its argument bytes in hex
//! {raw:81}                text bytes the encoding can't round trip
//! {{ and }}               a literal brace
//! ```
//!
//! [`render`] then [`parse`] gives back the same segments, so a patch never
//! changes text nobody edited.
//!
//! [`parse`] reads past every problem it finds and returns them all, each
//! with the byte range of the string it covers, so an editor can mark each
//! one where it was typed.

use std::fmt::Write;
use std::ops::Range;

use encoding_rs::{SHIFT_JIS, WINDOWS_1252};
use tpmt_message::{Encoding, TextSegment};
use tpmt_report::Diagnostic;
use tpmt_tables::message::tag::{self, Args, Tag, group};
use tpmt_tables::{Edition, entries, find};

/// A problem [`parse`] found, and the bytes of the string it covers.
pub type TextDiagnostic = Diagnostic<TextError, Range<usize>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    #[error("`{0}` is not a tag")]
    UnknownTag(String),

    #[error("`{0}` can't be written in {1}")]
    Unencodable(String, Encoding),

    #[error("`{0}` is not a valid argument for {1}")]
    BadArgument(String, &'static str),

    #[error("a `{{` is never closed")]
    Unclosed,

    #[error("a `}}` closes nothing; write `}}}}` for a literal one")]
    StrayClose,

    #[error("`{0}` is not hex")]
    BadHex(String),
}

/// `segments` as a string of text and tokens.
#[must_use]
pub fn render(segments: &[TextSegment], encoding: Encoding, edition: Edition) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            TextSegment::Text(bytes) => render_text(bytes, encoding, &mut out),
            TextSegment::Tag { group, code, args } => {
                out.push('{');
                if let Some(token) = named(*group, *code, args, encoding, edition) {
                    out.push_str(&token);
                } else {
                    let _ = write!(out, "#{group}.{code}");
                    if !args.is_empty() {
                        out.push(':');
                        out.push_str(&hex(args));
                    }
                }
                out.push('}');
            }
        }
    }
    out
}

/// A string from [`render`] back into segments.
///
/// # Errors
///
/// Every token that names no tag or carries an argument it can't, every
/// character the encoding has no bytes for, and every brace that doesn't
/// pair up. All of them are errors.
pub fn parse(
    text: &str,
    encoding: Encoding,
    edition: Edition,
) -> Result<Vec<TextSegment>, Vec<TextDiagnostic>> {
    let mut segments = Vec::new();
    let mut run = Vec::new();
    let mut found = Vec::new();
    let mut rest = text;
    // Where `rest` starts in `text`.
    let mut at = 0;
    while let Some(brace) = rest.find(['{', '}']) {
        let (before, from) = rest.split_at(brace);
        encode_run(before, at, encoding, &mut run, &mut found);
        let open = at + brace;
        if let Some(after) = from.strip_prefix("{{").or_else(|| from.strip_prefix("}}")) {
            encode_run(from.split_at(1).0, open, encoding, &mut run, &mut found);
            (rest, at) = (after, open + 2);
            continue;
        }
        let Some(opened) = from.strip_prefix('{') else {
            found.push(Diagnostic::error(TextError::StrayClose, open..open + 1));
            (rest, at) = (from.split_at(1).1, open + 1);
            continue;
        };
        let Some((token, after)) = opened.split_once('}') else {
            found.push(Diagnostic::error(TextError::Unclosed, open..text.len()));
            (rest, at) = ("", text.len());
            break;
        };
        let span = open..open + token.len() + 2;
        (rest, at) = (after, span.end);
        if let Some(bytes) = token.strip_prefix("raw:") {
            match unhex(bytes) {
                Ok(bytes) => run.extend(bytes),
                Err(error) => found.push(Diagnostic::error(error, span)),
            }
            continue;
        }
        if !run.is_empty() {
            segments.push(TextSegment::Text(std::mem::take(&mut run).into()));
        }
        match parse_tag(token, encoding, edition) {
            Ok(tag) => segments.push(tag),
            Err(error) => found.push(Diagnostic::error(error, span)),
        }
    }
    encode_run(rest, at, encoding, &mut run, &mut found);
    if !run.is_empty() {
        segments.push(TextSegment::Text(run.into()));
    }
    if found.is_empty() {
        Ok(segments)
    } else {
        Err(found)
    }
}

/// Appends `text` encoded to `run`, where `text` starts `at` bytes into the
/// string being parsed. A character the encoding can't hold is left out and
/// goes in `found`.
fn encode_run(
    text: &str,
    at: usize,
    encoding: Encoding,
    run: &mut Vec<u8>,
    found: &mut Vec<TextDiagnostic>,
) {
    if let Ok(bytes) = encode(text, encoding) {
        run.extend(bytes);
        return;
    }
    let mut buffer = [0; 4];
    for (offset, char) in text.char_indices() {
        let char = char.encode_utf8(&mut buffer);
        match encode(char, encoding) {
            Ok(bytes) => run.extend(bytes),
            Err(error) => found.push(Diagnostic::error(
                error,
                at + offset..at + offset + char.len(),
            )),
        }
    }
}

/// Appends `bytes` decoded, escaping braces. A character that doesn't come
/// back as the same bytes goes in as `{raw:..}`.
fn render_text(bytes: &[u8], encoding: Encoding, out: &mut String) {
    if let Some(text) = round_trip(bytes, encoding) {
        escape(&text, out);
        return;
    }
    let mut raw = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (char, after) = rest.split_at(char_len(rest, encoding).min(rest.len()));
        match round_trip(char, encoding) {
            Some(text) => {
                flush_raw(&mut raw, out);
                escape(&text, out);
            }
            None => raw.extend_from_slice(char),
        }
        rest = after;
    }
    flush_raw(&mut raw, out);
}

fn flush_raw(raw: &mut Vec<u8>, out: &mut String) {
    if !raw.is_empty() {
        out.push_str("{raw:");
        out.push_str(&hex(raw));
        out.push('}');
        raw.clear();
    }
}

fn escape(text: &str, out: &mut String) {
    for char in text.chars() {
        match char {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(char),
        }
    }
}

/// `bytes` decoded, when encoding the result gives `bytes` back.
fn round_trip(bytes: &[u8], encoding: Encoding) -> Option<String> {
    let text = decode(bytes, encoding)?;
    (encode(&text, encoding).ok()? == bytes).then_some(text)
}

/// How many bytes the character `bytes` opens with takes.
const fn char_len(bytes: &[u8], encoding: Encoding) -> usize {
    let Some(&lead) = bytes.first() else {
        return 0;
    };
    match encoding {
        Encoding::Legacy | Encoding::ShiftJis => match lead {
            0x81..=0x9F | 0xE0..=0xFC => 2,
            _ => 1,
        },
        Encoding::Windows1252 => 1,
        Encoding::Utf16Be => match lead {
            0xD8..=0xDB => 4,
            _ => 2,
        },
        Encoding::Utf8 => match lead {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        },
    }
}

fn decode(bytes: &[u8], encoding: Encoding) -> Option<String> {
    match encoding {
        Encoding::Legacy | Encoding::ShiftJis => SHIFT_JIS
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(Into::into),
        Encoding::Windows1252 => WINDOWS_1252
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(Into::into),
        Encoding::Utf16Be => {
            let units = bytes.chunks(2).map(|pair| match pair {
                [high, low] => Some(u16::from_be_bytes([*high, *low])),
                _ => None,
            });
            char::decode_utf16(units.collect::<Option<Vec<_>>>()?)
                .collect::<Result<_, _>>()
                .ok()
        }
        Encoding::Utf8 => std::str::from_utf8(bytes).ok().map(Into::into),
    }
}

fn encode(text: &str, encoding: Encoding) -> Result<Vec<u8>, TextError> {
    let encoded = match encoding {
        Encoding::Legacy | Encoding::ShiftJis => SHIFT_JIS.encode(text),
        Encoding::Windows1252 => WINDOWS_1252.encode(text),
        Encoding::Utf16Be => return Ok(text.encode_utf16().flat_map(u16::to_be_bytes).collect()),
        Encoding::Utf8 => return Ok(text.as_bytes().to_vec()),
    };
    match encoded {
        (_, _, true) => Err(TextError::Unencodable(text.to_string(), encoding)),
        (bytes, _, false) => Ok(bytes.into_owned()),
    }
}

/// The tag as a named token, without its braces, or `None` when it needs the
/// raw form.
fn named(
    group: u8,
    code: u16,
    args: &[u8],
    encoding: Encoding,
    edition: Edition,
) -> Option<String> {
    let tag = tag::find(group, code, edition)?;
    if matches!(group, group::SOUND | group::CAMERA) {
        return args.is_empty().then(|| format!("{}:{code}", tag.name));
    }
    let name = tag.name;
    match (tag.args, args) {
        (Args::None, []) => Some(name.to_string()),
        (Args::U8, [value]) => {
            let value = tag
                .values
                .and_then(|values| find(values, *value, edition))
                .map_or_else(|| value.to_string(), |entry| entry.name.to_string());
            Some(format!("{name}:{value}"))
        }
        (Args::U16, [high, low]) => Some(format!("{name}:{}", u16::from_be_bytes([*high, *low]))),
        (Args::U32, &[a, b, c, d]) => Some(format!("{name}:{}", u32::from_be_bytes([a, b, c, d]))),
        (Args::Ruby, [count, reading @ ..]) => {
            let reading =
                round_trip(reading, encoding).filter(|text| !text.contains(['{', '}']))?;
            Some(format!("{name}:{count}:{reading}"))
        }
        _ => None,
    }
}

fn parse_tag(token: &str, encoding: Encoding, edition: Edition) -> Result<TextSegment, TextError> {
    if let Some(raw) = token.strip_prefix('#') {
        return parse_raw_tag(raw).ok_or_else(|| TextError::UnknownTag(token.to_string()));
    }
    let (name, arg) = token
        .split_once(':')
        .map_or((token, None), |(name, arg)| (name, Some(arg)));
    let tag = by_name(name, edition).ok_or_else(|| TextError::UnknownTag(name.to_string()))?;
    let bad = || TextError::BadArgument(arg.unwrap_or_default().to_string(), tag.name);

    if matches!(tag.group, group::SOUND | group::CAMERA) {
        let code = arg.and_then(|arg| arg.parse().ok()).ok_or_else(bad)?;
        return Ok(TextSegment::Tag {
            group: tag.group,
            code,
            args: Box::default(),
        });
    }
    let args: Vec<u8> = match (tag.args, arg) {
        (Args::None, None) => Vec::new(),
        (Args::U8, Some(arg)) => {
            let named = tag
                .values
                .and_then(|values| entries(values, edition).find(|entry| entry.name == arg));
            let value = match named {
                Some(entry) => entry.value,
                None => arg.parse().map_err(|_| bad())?,
            };
            vec![value]
        }
        (Args::U16, Some(arg)) => arg
            .parse::<u16>()
            .map_err(|_| bad())?
            .to_be_bytes()
            .to_vec(),
        (Args::U32, Some(arg)) => arg
            .parse::<u32>()
            .map_err(|_| bad())?
            .to_be_bytes()
            .to_vec(),
        (Args::Ruby, Some(arg)) => {
            let (count, reading) = arg.split_once(':').ok_or_else(bad)?;
            let mut args = vec![count.parse().map_err(|_| bad())?];
            args.extend(encode(reading, encoding)?);
            args
        }
        _ => return Err(bad()),
    };
    Ok(TextSegment::Tag {
        group: tag.group,
        code: tag.code,
        args: args.into(),
    })
}

/// `group.code` and optional `:hex` arguments.
fn parse_raw_tag(raw: &str) -> Option<TextSegment> {
    let (id, args) = raw
        .split_once(':')
        .map_or((raw, ""), |(id, args)| (id, args));
    let (group, code) = id.split_once('.')?;
    Some(TextSegment::Tag {
        group: group.parse().ok()?,
        code: code.parse().ok()?,
        args: unhex(args).ok()?.into(),
    })
}

fn by_name(name: &str, edition: Edition) -> Option<&'static Tag> {
    [&tag::SOUND, &tag::CAMERA]
        .into_iter()
        .chain(tag::TAGS)
        .filter(|tag| tag.versions.contains(edition.version()))
        .find(|tag| tag.name == name)
}

/// Uppercase hex, two digits a byte.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02X}");
        out
    })
}

/// Bytes back out of hex, spaces ignored.
///
/// # Errors
///
/// [`TextError::BadHex`] for an odd digit count or a non-hex character.
pub fn unhex(text: &str) -> Result<Vec<u8>, TextError> {
    let digits: Vec<char> = text.chars().filter(|char| !char.is_whitespace()).collect();
    let bad = || TextError::BadHex(text.to_string());
    digits
        .chunks(2)
        .map(|pair| match pair {
            [high, low] => {
                let digit = |char: &char| char.to_digit(16);
                let byte = digit(high).ok_or_else(bad)? << 4 | digit(low).ok_or_else(bad)?;
                u8::try_from(byte).map_err(|_| bad())
            }
            _ => Err(bad()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tpmt_tables::Version;

    use super::*;

    const USA: Edition = Edition::default_language(Version::GcnUsa);

    fn text(bytes: &[u8]) -> TextSegment {
        TextSegment::Text(bytes.into())
    }

    fn tag(group: u8, code: u16, args: &[u8]) -> TextSegment {
        TextSegment::Tag {
            group,
            code,
            args: args.into(),
        }
    }

    fn round_trips(segments: &[TextSegment], expected: &str) {
        let rendered = render(segments, Encoding::ShiftJis, USA);
        assert_eq!(rendered, expected);
        assert_eq!(parse(&rendered, Encoding::ShiftJis, USA).unwrap(), segments);
    }

    #[test]
    fn tags_are_named_where_the_table_names_them() {
        round_trips(
            &[
                text(b"Hi "),
                tag(0, 0, &[]),
                text(b"!\n"),
                tag(255, 0, &[1]),
                tag(0, 7, &[0, 30]),
                tag(1, 20, &[]),
                tag(0, 40, &[0, 0, 1, 0]),
            ],
            "Hi {Player name}!\n{Color:Red}{Pause:30}{Sound:20}{Demo box:256}",
        );
    }

    #[test]
    fn what_the_table_doesnt_fit_stays_raw() {
        round_trips(
            &[tag(7, 3, &[1, 2]), tag(0, 0, &[9]), tag(255, 0, &[])],
            "{#7.3:0102}{#0.0:09}{#255.0}",
        );
    }

    #[test]
    fn braces_are_doubled() {
        round_trips(&[text(b"{a} }{")], "{{a}} }}{{");
    }

    #[test]
    fn shift_jis_text_and_ruby_round_trip() {
        // "剣" with the reading "けん".
        let sword = [0x8C, 0x95];
        let reading = [0x82, 0xAF, 0x82, 0xF1];
        let mut ruby = vec![1];
        ruby.extend(reading);
        round_trips(&[tag(255, 2, &ruby), text(&sword)], "{Ruby:1:けん}剣");
    }

    #[test]
    fn bytes_the_encoding_cant_hold_are_raw() {
        // A lead byte with nothing after it.
        round_trips(&[text(&[b'a', 0x81])], "a{raw:81}");
    }

    #[test]
    fn a_raw_run_joins_the_text_around_it() {
        assert_eq!(
            parse("a{raw:42}c", Encoding::ShiftJis, USA).unwrap(),
            [text(b"aBc")]
        );
    }

    #[test]
    fn bad_tokens_are_refused() {
        let parse = |text| parse(text, Encoding::ShiftJis, USA);
        let one = |error, span| Err(vec![Diagnostic::error(error, span)]);
        assert_eq!(
            parse("{Nope}"),
            one(TextError::UnknownTag("Nope".into()), 0..6)
        );
        assert_eq!(
            parse("{Pause:x}"),
            one(TextError::BadArgument("x".into(), "Pause"), 0..9)
        );
        assert_eq!(parse("{Pause"), one(TextError::Unclosed, 0..6));
        assert_eq!(parse("a}b"), one(TextError::StrayClose, 1..2));
        assert_eq!(
            parse("😀"),
            one(
                TextError::Unencodable("😀".into(), Encoding::ShiftJis),
                0..4
            )
        );
    }

    /// One bad token doesn't hide the next, and each points at itself.
    #[test]
    fn every_problem_is_found() {
        let errors = parse("a😀b{Nope}c}{Pause:30}{raw:G}", Encoding::ShiftJis, USA).unwrap_err();
        let spans: Vec<_> = errors.into_iter().map(|found| found.at).collect();
        assert_eq!(spans, [1..5, 6..12, 13..14, 24..31]);
    }

    /// A name picks one tag on an edition, or tokens would be ambiguous.
    #[test]
    fn tag_names_are_unique_per_version() {
        for version in Version::ALL {
            let edition = Edition::default_language(version);
            let names: Vec<_> = [&tag::SOUND, &tag::CAMERA]
                .into_iter()
                .chain(tag::TAGS)
                .filter(|tag| tag.versions.contains(version))
                .map(|tag| tag.name)
                .collect();
            for name in &names {
                assert_eq!(
                    names.iter().filter(|other| *other == name).count(),
                    1,
                    "{name}"
                );
                assert!(by_name(name, edition).is_some());
            }
        }
    }
}
