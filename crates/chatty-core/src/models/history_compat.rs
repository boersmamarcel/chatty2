//! Reading a message history that was written before assistant content
//! carried a `type` tag (AGE-369).
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

use anyhow::{Context, Result};
use rig_core::completion::Message;
use serde_json::Value;

/// The error context a history that is neither shape fails with.
const NEITHER_SHAPE: &str =
    "message history is neither the tagged nor the legacy untagged content shape";

/// Parse a persisted `Vec<Message>` history, accepting legacy untagged text
/// content blocks alongside today's tagged ones.
pub fn parse_history(json: &str) -> Result<Vec<Message>> {
    let tagged_error = match serde_json::from_str::<Vec<Message>>(json) {
        Ok(history) => return Ok(history),
        Err(error) => error,
    };

    // Anything that is not the legacy shape — malformed JSON, an unknown
    // content block, a missing field — is reported as the tagged parse saw
    // it, since that error still points at the real offset in `json`.
    let Ok(mut value) = serde_json::from_str::<Value>(json) else {
        return Err(tagged_error).context(NEITHER_SHAPE);
    };
    if !tag_untagged_text_blocks(&mut value) {
        return Err(tagged_error).context(NEITHER_SHAPE);
    }

    serde_json::from_value(value).context(NEITHER_SHAPE)
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
            Message::Assistant { content, .. } => content
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
