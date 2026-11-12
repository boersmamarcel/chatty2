//! Reading a message history written by an older rig: before assistant
//! content carried a `type` tag (AGE-369), or by rig 0.42 (AGE-1151).
//!
//! rig's [`AssistantContent`](rig_core::completion::message::AssistantContent)
//! is internally tagged (`#[serde(tag = "type")]`) and the tag is required on
//! deserialize, but an older rig serialized the text variant tagless — a bare
//! `{"text": …}` block. Rows written then are still in every user's store: on
//! a copy of the author's real store, 81 of 121 conversations fail to restore
//! with `missing field \`type\`` and the whole conversation is unopenable.
//!
//! So the read path accepts both shapes. The tagged form is tried first and is
//! the only path a current row takes — an untagged block costs one extra parse
//! of that row and nothing else, and no row is rewritten: the write path stays
//! tagged, and a legacy row is re-tagged on disk only when its conversation is
//! saved again for its own reasons.
//!
//! # rig 0.42 histories (AGE-1151)
//!
//! chatty stores rig's own message JSON, and rig 0.46 reads rig 0.42's tool
//! calls, tool results and reasoning differently. A 0.42 history is upgraded
//! on load ([`upgrade_rig_042`]), one way and without a backup: it is written
//! back in 0.46's shape the next time its conversation is saved.
//!
//! - A tool call's id (a string, with the provider's id beside it) becomes
//!   0.46's `CallId`: the provider's id when it issued one, else the id rig
//!   0.42 minted, kept as the call's id. A result's `call` is mapped the same
//!   way, so every call and its result still pair.
//! - Reasoning blocks are dropped: 0.42's are a list of typed parts with
//!   signatures, and 0.46 would replay an unsigned block to another model as
//!   plain text. Answers and tool calls stay. An assistant turn that held
//!   only reasoning is dropped with it.

use anyhow::{Context, Result};
use rig_core::completion::Message;
use serde_json::Value;

/// The error context a history that is neither shape fails with.
const NEITHER_SHAPE: &str =
    "message history is neither the tagged nor the legacy untagged content shape";

/// Parse a persisted `Vec<Message>` history, accepting legacy untagged text
/// content blocks and rig 0.42's shapes alongside today's.
pub fn parse_history(json: &str) -> Result<Vec<Message>> {
    let tagged_error = match serde_json::from_str::<Vec<Message>>(json) {
        Ok(history) => return Ok(history),
        Err(error) => error,
    };

    // Anything that is not a legacy shape — malformed JSON, an unknown
    // content block, a missing field — is reported as the tagged parse saw
    // it, since that error still points at the real offset in `json`.
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Err(tagged_error).context(NEITHER_SHAPE);
    };
    upgrade(value).ok_or(tagged_error).context(NEITHER_SHAPE)?
}

/// [`parse_history`] for a history already read as JSON, such as the
/// messages a swarm trace recorded.
pub fn parse_history_value(value: Value) -> Result<Vec<Message>> {
    let current_error = match serde_json::from_value::<Vec<Message>>(value.clone()) {
        Ok(history) => return Ok(history),
        Err(error) => error,
    };
    upgrade(value).ok_or(current_error).context(NEITHER_SHAPE)?
}

/// The history `value` holds once its legacy shapes are upgraded, or `None`
/// when it holds none, so the caller reports its own parse error.
fn upgrade(mut value: Value) -> Option<Result<Vec<Message>>> {
    let tagged = tag_untagged_text_blocks(&mut value);
    let upgraded = upgrade_rig_042(&mut value);
    (tagged || upgraded).then(|| serde_json::from_value(value).context(NEITHER_SHAPE))
}

/// Rewrite rig 0.42's tool-call ids and drop its reasoning blocks (module
/// docs). Returns whether anything changed.
fn upgrade_rig_042(value: &mut Value) -> bool {
    let Some(messages) = value.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    let mut emptied = Vec::new();
    for (index, message) in messages.iter_mut().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|block| !is_rig_042_reasoning(block));
        if blocks.len() != before {
            changed = true;
            if blocks.is_empty() && role.as_deref() == Some("assistant") {
                emptied.push(index);
            }
        }
        for block in blocks.iter_mut().filter_map(Value::as_object_mut) {
            match block.get("type").and_then(Value::as_str) {
                Some("toolcall") => changed |= upgrade_call_id(block, "id"),
                Some("toolresult") => changed |= upgrade_call_id(block, "call"),
                _ => {}
            }
        }
    }
    for index in emptied.into_iter().rev() {
        messages.remove(index);
    }
    changed
}

/// Whether `block` is a rig 0.42 reasoning block: `{"type": "reasoning",
/// "content": [...]}`, where rig 0.46's carries a `text`.
fn is_rig_042_reasoning(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("reasoning")
        && !block.get("text").is_some_and(Value::is_string)
}

/// Turn a rig 0.42 call id under `key` (a string, with `provider` beside it)
/// into a rig 0.46 `CallId`; a call's 0.42-only fields go with it. Returns
/// whether it changed anything.
fn upgrade_call_id(block: &mut serde_json::Map<String, Value>, key: &str) -> bool {
    let Some(Value::String(id)) = block.get(key) else {
        return false;
    };
    let provider_id = block
        .get("provider")
        .and_then(|provider| provider.get("call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let id = provider_id.unwrap_or(id).to_owned();
    block.insert(key.to_owned(), serde_json::json!({ "provider": id }));
    for field in ["provider", "signature", "additional_params"] {
        block.remove(field);
    }
    true
}

/// Give every untagged `{"text": …}` content block the `text` tag it was
/// written without. Returns whether any block was rewritten, so a failure
/// that has nothing to do with the missing tag keeps its original error.
fn tag_untagged_text_blocks(value: &mut Value) -> bool {
    let Some(messages) = value.as_array_mut() else {
        return false;
    };
    let mut rewrote = false;
    for message in messages {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks {
            let Some(block) = block.as_object_mut() else {
                continue;
            };
            if block.contains_key("type") || !matches!(block.get("text"), Some(Value::String(_))) {
                continue;
            }
            block.insert("type".to_string(), Value::String("text".to_string()));
            rewrote = true;
        }
    }
    rewrote
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::AssistantContent;

    /// One conversation from the author's real store, structure intact and
    /// every string replaced with a placeholder: a tagged user turn followed
    /// by an assistant turn whose single content block has no `type`.
    const LEGACY_FIXTURE: &str = include_str!("snapshots/legacy_untagged_history.json");

    fn assistant_text(message: &Message) -> String {
        match message {
            Message::Assistant(rig_core::message::AssistantMessage { content, .. }) => content
                .iter()
                .map(|block| match block {
                    AssistantContent::Text(text) => text.text.clone(),
                    other => panic!("expected text content, got {other:?}"),
                })
                .collect(),
            other => panic!("expected an assistant message, got {other:?}"),
        }
    }

    #[test]
    fn the_captured_legacy_row_restores() {
        let history = parse_history(LEGACY_FIXTURE).expect("a legacy row restores");

        assert_eq!(history.len(), 2);
        assert!(matches!(history[0], Message::User { .. }));
        assert_eq!(assistant_text(&history[1]), "placeholder assistant reply");
    }

    #[test]
    fn a_tagged_history_still_restores() {
        let tagged = serde_json::to_string(&vec![
            Message::user("placeholder question"),
            Message::assistant("placeholder answer"),
        ])
        .expect("a current history serializes");

        let history = parse_history(&tagged).expect("a tagged row restores");

        assert_eq!(assistant_text(&history[1]), "placeholder answer");
    }

    #[test]
    fn a_block_that_is_neither_shape_fails_with_a_clear_error() {
        let neither = r#"[{"role":"assistant","id":null,"content":[{"whatever":1}]}]"#;

        let error = parse_history(neither).expect_err("an unknown content block is not accepted");

        let reported = format!("{error:#}");
        assert!(
            reported.contains(NEITHER_SHAPE) && reported.contains("missing field `type`"),
            "the error must name both the shape and the missing tag, got: {reported}"
        );
    }

    #[test]
    fn a_history_that_is_not_json_at_all_still_fails() {
        let error = parse_history("not json").expect_err("garbage is not accepted");

        let reported = format!("{error:#}");
        assert!(
            reported.contains(NEITHER_SHAPE) && reported.contains("at line 1 column"),
            "the error must keep serde's own position, got: {reported}"
        );
    }

    #[test]
    fn a_legacy_row_is_rewritten_tagged_on_the_next_save() {
        let history = parse_history(LEGACY_FIXTURE).expect("a legacy row restores");

        let rewritten = serde_json::to_string(&history).expect("it serializes");

        assert!(
            rewritten.contains(r#"{"type":"text","text":"placeholder assistant reply"}"#),
            "the write path stays tagged, got: {rewritten}"
        );
        assert_eq!(
            parse_history(&rewritten).expect("and reloads").len(),
            history.len()
        );
    }

    #[test]
    fn a_tagged_and_an_untagged_block_in_one_history_both_load() {
        let mixed = r#"[
            {"role":"assistant","id":null,"content":[{"text":"legacy"}]},
            {"role":"assistant","id":null,"content":[{"type":"text","text":"current"}]}
        ]"#;

        let history = parse_history(mixed).expect("a store written across the change restores");

        assert_eq!(assistant_text(&history[0]), "legacy");
        assert_eq!(assistant_text(&history[1]), "current");
    }
}
