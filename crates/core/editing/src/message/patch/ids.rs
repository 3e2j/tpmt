//! How a patch names vanilla items, and where new ones start.

use std::collections::HashMap;

use tpmt_message::{Bmg, MessageId};

use super::MessageKey;
use crate::message::edit::{next_message_id, next_node_id};

/// What [`diff`](fn@super::diff) and [`apply`](fn@super::apply) both need to
/// turn ids into patch names and back.
pub(super) struct Ids {
    /// Each vanilla message's key, by position.
    pub(super) keys: Vec<MessageKey>,
    pub(super) by_key: HashMap<MessageKey, MessageId>,
    /// The first id past every vanilla message, and every vanilla node. An
    /// id below is a vanilla item's, and one at or past is a new item's.
    pub(super) messages: u32,
    pub(super) nodes: u32,
}

impl Ids {
    /// `vanilla` must be a fresh decode, so its ids are its positions.
    pub(super) fn new(vanilla: &Bmg) -> Self {
        let mut counts: HashMap<u16, usize> = HashMap::new();
        for message in &vanilla.messages {
            *counts.entry(message.public_id).or_default() += 1;
        }
        let keys: Vec<MessageKey> = vanilla
            .messages
            .iter()
            .map(|message| match vanilla.mid1 {
                Some(_) if counts[&message.public_id] == 1 => MessageKey::Id(message.public_id),
                _ => MessageKey::Position(message.id.0),
            })
            .collect();
        let mut by_key: HashMap<MessageKey, MessageId> = vanilla
            .messages
            .iter()
            .map(|message| (MessageKey::Position(message.id.0), message.id))
            .collect();
        by_key.extend(
            keys.iter()
                .copied()
                .zip(vanilla.messages.iter().map(|message| message.id)),
        );
        Self {
            keys,
            by_key,
            messages: next_message_id(vanilla).0,
            nodes: next_node_id(vanilla).0,
        }
    }
}
