//! How a patch names vanilla and new items, each written as a TOML string.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How a patch names a vanilla message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MessageKey {
    /// Its MID1 id, written as the number, in a file where no two messages
    /// share one.
    Id(u16),
    /// Its vanilla position, written `@12`, in any other file.
    Position(u32),
}

/// A message a node shows: a vanilla one by its key, or a new one by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRef {
    Vanilla(MessageKey),
    New(String),
}

/// Where an edge goes: `"end"`, a vanilla node as `"node:87"`, or a new one by
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRef {
    End,
    Vanilla(u32),
    New(String),
}

/// A number as a TOML key, which must be a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Number<T>(pub T);

// What each reference looks like in TOML.

impl fmt::Display for MessageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Position(at) => write!(f, "@{at}"),
        }
    }
}

impl FromStr for MessageKey {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("`{text}` is not a message id or `@position`");
        let parsed = text.strip_prefix('@').map_or_else(
            || text.parse().map(Self::Id),
            |at| at.parse().map(Self::Position),
        );
        parsed.map_err(|_| bad())
    }
}

impl fmt::Display for MessageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vanilla(key) => key.fmt(f),
            Self::New(name) => f.write_str(name),
        }
    }
}

impl FromStr for MessageRef {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text.starts_with(|first: char| first.is_ascii_digit() || first == '@') {
            text.parse().map(Self::Vanilla)
        } else {
            Ok(Self::New(text.to_string()))
        }
    }
}

impl fmt::Display for NodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::End => f.write_str("end"),
            Self::Vanilla(at) => write!(f, "node:{at}"),
            Self::New(name) => f.write_str(name),
        }
    }
}

impl FromStr for NodeRef {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text == "end" {
            return Ok(Self::End);
        }
        text.strip_prefix("node:").map_or_else(
            || Ok(Self::New(text.to_string())),
            |at| {
                at.parse()
                    .map(Self::Vanilla)
                    .map_err(|_| format!("`{text}` is not `node:` and a position"))
            },
        )
    }
}

impl<T: fmt::Display> fmt::Display for Number<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: FromStr> FromStr for Number<T> {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        text.parse()
            .map(Self)
            .map_err(|_| format!("`{text}` is not a number"))
    }
}

/// Serde through `Display` and `FromStr`.
macro_rules! as_string {
    ($($type:ty),* $(,)?) => {$(
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    )*};
}

as_string!(MessageKey, MessageRef, NodeRef, Number<u32>, Number<u16>);
