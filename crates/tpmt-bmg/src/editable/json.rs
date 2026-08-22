//! The BMG translation layer: `Bmg` as JSON.
//!
//! Structural mirror only. No game-specific names are named here, since
//! what any of them mean is per-game and stays out of this crate. Byte blobs
//! whose layout is game data (attributes, tag bytes, event params) round-trip
//! as hex strings rather than number arrays, since a hex string is what a
//! project's diff and a modder's eye both read best.
//!
//! A message's tags sit inline in its text as `<hex>` rather than splitting
//! the line into a list of parts, so the text reads the way it displays. A
//! literal `\` or `<` is escaped with a leading `\`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sections::flow::{Flow, Node, NodeId, Root};
use crate::{Bmg, Encoding, Error, Message, MessageId, Mid1Header, Result, TextSegment};

/// The extension a project chains onto a BMG's own when it is written out in
/// this form, e.g. `zel_00.bmg` becomes `zel_00.bmg.json`.
pub const EXTENSION: &str = "json";

/// Encodes a `Bmg` as the bytes of its JSON translation layer, ready to write
/// to a file. Pretty printed: this is a file a modder reads and edits by hand.
pub fn encode(bmg: &Bmg) -> Vec<u8> {
    serde_json::to_vec_pretty(&to_json(bmg)).expect("a BMG translation document always serializes")
}

#[derive(Serialize, Deserialize)]
struct JsonBmg {
    encoding: String,
    attribute_len: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mid1: Option<JsonMid1>,
    messages: Vec<JsonMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flow: Option<JsonFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    strings: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra: Vec<JsonUnknownSection>,
}

#[derive(Serialize, Deserialize)]
struct JsonMid1 {
    ordered: bool,
    form: u8,
    shift_bytes: u8,
}

#[derive(Serialize, Deserialize)]
struct JsonUnknownSection {
    magic: String,
    data: String,
}

#[derive(Serialize, Deserialize)]
struct JsonFlow {
    roots: Vec<JsonRoot>,
    nodes: Vec<JsonNode>,
}

#[derive(Serialize, Deserialize)]
struct JsonRoot {
    public_id: u16,
    node: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum JsonNode {
    Text {
        id: u32,
        message: u32,
        next: Option<u32>,
    },
    Branch {
        id: u32,
        query: u16,
        param: u16,
        children: Vec<Option<u32>>,
    },
    Event {
        id: u32,
        event: u8,
        params: String,
        next: Option<u32>,
    },
}

#[derive(Serialize, Deserialize)]
struct JsonMessage {
    public_id: u16,
    id: u32,
    attributes: String,
    text: String,
}

/// Turns a `Bmg` into its JSON translation layer.
pub fn to_json(bmg: &Bmg) -> Value {
    let json = JsonBmg {
        encoding: bmg.encoding.as_str().to_string(),
        attribute_len: bmg.attribute_len,
        mid1: bmg.mid1.map(mid1_to_json),
        messages: bmg
            .messages
            .iter()
            .map(|m| message_to_json(m, bmg.encoding))
            .collect(),
        flow: bmg.flow.as_ref().map(flow_to_json),
        strings: bmg.strings.as_ref().map(|strings| {
            strings
                .iter()
                .map(|s| decode_text(s, bmg.encoding))
                .collect()
        }),
        extra: bmg.extra.iter().map(unknown_section_to_json).collect(),
    };
    serde_json::to_value(json).expect("a JsonBmg always serializes")
}

/// Turns a translation layer document back into a `Bmg`.
pub fn from_json(value: &Value) -> Result<Bmg> {
    let json: JsonBmg = serde_json::from_value(value.clone())?;
    let encoding = encoding_from_str(&json.encoding)?;
    Ok(Bmg {
        encoding,
        attribute_len: json.attribute_len,
        mid1: json.mid1.map(mid1_from_json),
        messages: json
            .messages
            .into_iter()
            .map(|m| message_from_json(m, encoding))
            .collect::<Result<Vec<_>>>()?,
        flow: json.flow.map(flow_from_json).transpose()?,
        strings: json
            .strings
            .map(|strings| {
                strings
                    .iter()
                    .map(|s| encode_text(s, encoding))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?,
        extra: json
            .extra
            .into_iter()
            .map(unknown_section_from_json)
            .collect::<Result<Vec<_>>>()?,
    })
}

fn mid1_to_json(mid1: Mid1Header) -> JsonMid1 {
    JsonMid1 {
        ordered: mid1.ordered,
        form: mid1.form,
        shift_bytes: mid1.shift_bytes,
    }
}

fn mid1_from_json(json: JsonMid1) -> Mid1Header {
    Mid1Header {
        ordered: json.ordered,
        form: json.form,
        shift_bytes: json.shift_bytes,
    }
}

fn unknown_section_to_json(section: &crate::UnknownSection) -> JsonUnknownSection {
    JsonUnknownSection {
        magic: to_hex(&section.magic),
        data: to_hex(&section.data),
    }
}

fn unknown_section_from_json(json: JsonUnknownSection) -> Result<crate::UnknownSection> {
    let magic = from_hex(&json.magic)?
        .try_into()
        .map_err(|_| Error::Corrupt("a section magic is not four bytes"))?;
    Ok(crate::UnknownSection {
        magic,
        data: from_hex(&json.data)?,
    })
}

fn flow_to_json(flow: &Flow) -> JsonFlow {
    JsonFlow {
        roots: flow
            .roots
            .iter()
            .map(|r| JsonRoot {
                public_id: r.public_id,
                node: r.node.0,
            })
            .collect(),
        nodes: flow.nodes.iter().map(node_to_json).collect(),
    }
}

fn flow_from_json(json: JsonFlow) -> Result<Flow> {
    Ok(Flow {
        nodes: json
            .nodes
            .into_iter()
            .map(node_from_json)
            .collect::<Result<Vec<_>>>()?,
        roots: json
            .roots
            .into_iter()
            .map(|r| Root {
                public_id: r.public_id,
                node: NodeId(r.node),
            })
            .collect(),
    })
}

fn node_to_json(node: &Node) -> JsonNode {
    match node {
        Node::Text { id, message, next } => JsonNode::Text {
            id: id.0,
            message: message.0,
            next: next.map(|n| n.0),
        },
        Node::Branch {
            id,
            query,
            param,
            children,
        } => JsonNode::Branch {
            id: id.0,
            query: *query,
            param: *param,
            children: children.iter().map(|c| c.map(|n| n.0)).collect(),
        },
        Node::Event {
            id,
            event,
            params,
            next,
        } => JsonNode::Event {
            id: id.0,
            event: *event,
            params: to_hex(params),
            next: next.map(|n| n.0),
        },
    }
}

fn node_from_json(json: JsonNode) -> Result<Node> {
    Ok(match json {
        JsonNode::Text { id, message, next } => Node::Text {
            id: NodeId(id),
            message: MessageId(message),
            next: next.map(NodeId),
        },
        JsonNode::Branch {
            id,
            query,
            param,
            children,
        } => Node::Branch {
            id: NodeId(id),
            query,
            param,
            children: children.into_iter().map(|c| c.map(NodeId)).collect(),
        },
        JsonNode::Event {
            id,
            event,
            params,
            next,
        } => Node::Event {
            id: NodeId(id),
            event,
            params: from_hex(&params)?
                .try_into()
                .map_err(|_| Error::Corrupt("an event's params are not four bytes"))?,
            next: next.map(NodeId),
        },
    })
}

fn encoding_from_str(s: &str) -> Result<Encoding> {
    [
        Encoding::Legacy,
        Encoding::Windows1252,
        Encoding::Utf16Be,
        Encoding::ShiftJis,
        Encoding::Utf8,
    ]
    .into_iter()
    .find(|e| e.as_str() == s)
    .ok_or(Error::Corrupt("not a known encoding name"))
}

fn message_to_json(message: &Message, encoding: Encoding) -> JsonMessage {
    JsonMessage {
        public_id: message.public_id,
        id: message.id.0,
        attributes: to_hex(&message.attributes),
        text: text_to_json(&message.text, encoding),
    }
}

fn message_from_json(json: JsonMessage, encoding: Encoding) -> Result<Message> {
    Ok(Message {
        public_id: json.public_id,
        id: MessageId(json.id),
        attributes: from_hex(&json.attributes)?,
        text: text_from_json(&json.text, encoding)?,
    })
}

/// Turns a message's segments into one line: a tag sits inline as `<hex>`,
/// and a literal `\` or `<` is escaped with a leading `\` so it is never
/// mistaken for one.
fn text_to_json(segments: &[TextSegment], encoding: Encoding) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            TextSegment::Text(bytes) => {
                for ch in decode_text(bytes, encoding).chars() {
                    if ch == '\\' || ch == '<' {
                        out.push('\\');
                    }
                    out.push(ch);
                }
            }
            TextSegment::Tag(bytes) => {
                out.push('<');
                out.push_str(&to_hex(bytes));
                out.push('>');
            }
        }
    }
    out
}

/// The inverse of [`text_to_json`].
fn text_from_json(s: &str, encoding: Encoding) -> Result<Vec<TextSegment>> {
    let mut segments = Vec::new();
    let mut text = String::new();
    let mut chars = s.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(escaped @ ('\\' | '<')) => text.push(escaped),
                Some(_) => {
                    return Err(Error::Corrupt(
                        "a message's text escapes a character other than '\\' or '<'",
                    ));
                }
                None => return Err(Error::Corrupt("a message's text ends mid escape")),
            },
            '<' => {
                if !text.is_empty() {
                    segments.push(TextSegment::Text(encode_text(&text, encoding)?));
                    text.clear();
                }
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('>') => break,
                        Some(c) => hex.push(c),
                        None => return Err(Error::Corrupt("a tag is missing its closing '>'")),
                    }
                }
                segments.push(TextSegment::Tag(from_hex(&hex)?));
            }
            _ => text.push(ch),
        }
    }
    if !text.is_empty() {
        segments.push(TextSegment::Text(encode_text(&text, encoding)?));
    }
    Ok(segments)
}

fn encoding_rs_encoding(encoding: Encoding) -> &'static encoding_rs::Encoding {
    match encoding {
        Encoding::Legacy | Encoding::ShiftJis => encoding_rs::SHIFT_JIS,
        Encoding::Windows1252 => encoding_rs::WINDOWS_1252,
        Encoding::Utf16Be => encoding_rs::UTF_16BE,
        Encoding::Utf8 => encoding_rs::UTF_8,
    }
}

fn decode_text(bytes: &[u8], encoding: Encoding) -> String {
    let (text, _, _) = encoding_rs_encoding(encoding).decode(bytes);
    text.into_owned()
}

fn encode_text(text: &str, encoding: Encoding) -> Result<Vec<u8>> {
    let (bytes, _, had_errors) = encoding_rs_encoding(encoding).encode(text);
    if had_errors {
        return Err(Error::Corrupt(
            "text does not fit in the message file's encoding",
        ));
    }
    Ok(bytes.into_owned())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(Error::Corrupt("a hex string has an odd number of digits"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| Error::Corrupt("a hex string contains a non-hex digit"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bmg, Encoding, Message, MessageId, TextSegment};

    /// The shape this whole change is about: a tag sits inline in the text
    /// as `<hex>` instead of splitting the line into a list of parts.
    #[test]
    fn tags_are_inlined_in_their_text_as_hex() {
        let text = vec![
            TextSegment::Text(b"Hi ".to_vec()),
            TextSegment::Tag(vec![0x1A, 4, 0x01, 0x02]),
        ];

        assert_eq!(text_to_json(&text, Encoding::ShiftJis), "Hi <1a040102>");
        assert_eq!(
            text_from_json("Hi <1a040102>", Encoding::ShiftJis).unwrap(),
            text
        );
    }

    /// A literal `<` or `\` in the text is escaped, so it is never mistaken
    /// for where a tag starts.
    #[test]
    fn a_literal_angle_bracket_or_backslash_round_trips() {
        let text = vec![TextSegment::Text(b"5 < 10 \\ ok".to_vec())];

        let json = text_to_json(&text, Encoding::ShiftJis);
        assert_eq!(text_from_json(&json, Encoding::ShiftJis).unwrap(), text);
    }

    /// A backslash before anything but `\` or `<` is corrupt rather than
    /// silently dropped: `text_to_json` never emits that pairing, so it can
    /// only come from a hand edit, and dropping the backslash would corrupt
    /// the text instead of reporting the mistake.
    #[test]
    fn a_backslash_before_an_unescapable_character_is_corrupt() {
        assert!(matches!(
            text_from_json("C:\\name", Encoding::ShiftJis),
            Err(Error::Corrupt(_))
        ));
    }

    /// A tag missing its closing `>` is corrupt rather than silently
    /// swallowing the rest of the line.
    #[test]
    fn an_unterminated_tag_is_corrupt() {
        assert!(matches!(
            text_from_json("Hi <1a04", Encoding::ShiftJis),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn a_bmg_with_one_message_round_trips_through_json() {
        let bmg = Bmg {
            encoding: Encoding::ShiftJis,
            attribute_len: 6,
            mid1: None,
            messages: vec![Message {
                public_id: 0,
                id: MessageId(0),
                attributes: vec![0xAA, 0xBB],
                text: vec![TextSegment::Text(b"Hi".to_vec())],
            }],
            flow: None,
            strings: None,
            extra: Vec::new(),
        };

        let json = crate::editable::json::to_json(&bmg);
        let round_tripped = crate::editable::json::from_json(&json).unwrap();

        assert_eq!(round_tripped, bmg);
    }

    /// Everything the minimal case left out: MID1, STR1, an unknown section,
    /// a tag segment, and a flow graph covering all three node types.
    #[test]
    fn a_bmg_with_flow_mid1_strings_and_extra_round_trips_through_json() {
        use crate::UnknownSection;
        use crate::sections::flow::{Flow, Node, NodeId, Root};
        use crate::sections::message::Mid1Header;

        let bmg = Bmg {
            encoding: Encoding::ShiftJis,
            attribute_len: 4,
            mid1: Some(Mid1Header {
                ordered: true,
                form: 0,
                shift_bytes: 0,
            }),
            messages: vec![Message {
                public_id: 42,
                id: MessageId(0),
                attributes: vec![],
                text: vec![
                    TextSegment::Text(b"Hi ".to_vec()),
                    TextSegment::Tag(vec![0x1A, 4, 0x01, 0x02]),
                ],
            }],
            flow: Some(Flow {
                nodes: vec![
                    Node::Text {
                        id: NodeId(0),
                        message: MessageId(0),
                        next: Some(NodeId(1)),
                    },
                    Node::Branch {
                        id: NodeId(1),
                        query: 3,
                        param: 0,
                        children: vec![Some(NodeId(2)), None],
                    },
                    Node::Event {
                        id: NodeId(2),
                        event: 9,
                        params: [0x0a, 0x00, 0x00, 0x00],
                        next: None,
                    },
                ],
                roots: vec![Root {
                    public_id: 3000,
                    node: NodeId(0),
                }],
            }),
            strings: Some(vec![b"arrow".to_vec(), b"arrows".to_vec()]),
            extra: vec![UnknownSection {
                magic: *b"XXXX",
                data: vec![0xde, 0xad],
            }],
        };

        let json = crate::editable::json::to_json(&bmg);
        let round_tripped = crate::editable::json::from_json(&json).unwrap();

        assert_eq!(round_tripped, bmg);
    }
}
