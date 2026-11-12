//! Is one provider request an append-only extension of another? (AGE-277)
//!
//! Prompt caching is a byte-prefix property, so the comparison is on the
//! bytes a client sent: [`first_rewritten_message`] finds the first message
//! of an earlier request that a later one does not carry byte for byte.
//! [`without_breakpoints`] strips the `cache_control` markers the OpenRouter
//! path moves on purpose (AGE-205), for comparing on billed content only.
//! Shared by chatty-core's turn-to-turn test and the swarm kit's resumed
//! handle test (RC-3).

use similar::TextDiff;

/// The `messages` array of `body`, one element per message, as the bytes rig
/// sent them.
///
/// Each element is sliced out of the body with `RawValue` rather than parsed
/// and re-serialized: this workspace's `serde_json` has no `preserve_order`,
/// so a round trip would sort keys and turn this into a structural comparison.
/// Prefix caching is byte-level, and so is this.
pub fn message_elements(body: &[u8]) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Body<'a> {
        #[serde(borrow)]
        messages: Vec<&'a serde_json::value::RawValue>,
    }
    let request: Body<'_> = serde_json::from_slice(body).expect("the request body is JSON");
    request
        .messages
        .iter()
        .map(|message| message.get().to_string())
        .collect()
}

/// `later` must contain `earlier`'s messages as an exact leading run.
pub fn assert_append_only(earlier: &[u8], later: &[u8]) {
    if let Some(Rewrite { index, diff }) = first_rewritten_message(earlier, later) {
        panic!(
            "message {index} was rewritten between turns, so every cache \
             block after it is invalidated:\n{diff}"
        );
    }
}

/// The first message of `earlier` that `later` does not carry byte for byte.
pub struct Rewrite {
    pub index: usize,
    pub diff: String,
}

/// The first message `later` rewrote, or `None` when `later` extends `earlier`.
pub fn first_rewritten_message(earlier: &[u8], later: &[u8]) -> Option<Rewrite> {
    let earlier = message_elements(earlier);
    let later = message_elements(later);

    assert!(
        later.len() >= earlier.len(),
        "turn N+1 sent {} messages, fewer than turn N's {}",
        later.len(),
        earlier.len()
    );

    earlier
        .iter()
        .zip(later.iter())
        .enumerate()
        .find(|(_, (before, after))| before != after)
        .map(|(index, (before, after))| Rewrite {
            index,
            diff: message_diff(before, after),
        })
}

/// Indexes of the messages carrying a `cache_control` marker on any content
/// block: rig's system breakpoint and this layer's moving one (AGE-205).
pub fn breakpoint_indexes(body: &[u8]) -> Vec<usize> {
    let request: serde_json::Value = serde_json::from_slice(body).expect("JSON body");
    request["messages"]
        .as_array()
        .expect("a messages array")
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| b.get("cache_control").is_some()))
        })
        .map(|(index, _)| index)
        .collect()
}

/// `body` with every `cache_control` marker removed and a one-block text
/// content folded back to the string it was before the marker was added, so
/// two requests can be compared on the content the provider bills for.
pub fn without_breakpoints(body: &[u8]) -> Vec<u8> {
    let mut request: serde_json::Value = serde_json::from_slice(body).expect("JSON body");
    for message in request["messages"]
        .as_array_mut()
        .expect("a messages array")
    {
        let Some(blocks) = message["content"].as_array_mut() else {
            continue;
        };
        for block in blocks.iter_mut() {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
        if let [block] = blocks.as_slice()
            && block["type"] == "text"
            && let Some(text) = block["text"].as_str()
        {
            message["content"] = serde_json::Value::String(text.to_string());
        }
    }
    serde_json::to_vec(&request).expect("the body re-serializes")
}

/// A unified diff of one rewritten message. Split on `,` as well as newlines so
/// a compact one-line JSON message still diffs readably.
pub fn message_diff(before: &str, after: &str) -> String {
    let before = before.replace(',', ",\n");
    let after = after.replace(',', ",\n");
    let diff = TextDiff::from_lines(&before, &after);
    let mut unified = diff.unified_diff();
    unified.header("turn N", "turn N+1").to_string()
}
