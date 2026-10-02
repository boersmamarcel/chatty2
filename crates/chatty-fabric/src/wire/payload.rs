//! Wire-owned mirrors of the payloads chatty-core owns (ADR-0021 § 1, EN-3a).
//!
//! The broker reads usage off a task's terminal metadata (its ledger and
//! the swarm forwarding do), and passes a callee's progress and metadata on
//! to the caller. So the wire defines their shape here, and chatty-core
//! converts its own types to and from these at its edge with `From` impls.
//! The lenient `Value` usage parser stays only on the A2A edge, for peers
//! that are not chatty.

use serde::{Deserialize, Serialize};

use super::Opaque;

/// A task's token usage: the four totals and one line per model (AGE-682).
/// The totals are the lines summed, for a reader that only wants the
/// number; nothing on it is a price.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub lines: Vec<WireUsageLine>,
}

impl WireUsage {
    /// Usage made of `lines`, with the totals summed from them.
    pub fn from_lines(lines: Vec<WireUsageLine>) -> Self {
        let sum = |bucket: fn(&WireUsageLine) -> u32| {
            lines
                .iter()
                .fold(0u32, |total, line| total.saturating_add(bucket(line)))
        };
        Self {
            input_tokens: sum(|l| l.input_tokens),
            output_tokens: sum(|l| l.output_tokens),
            cache_read_tokens: sum(|l| l.cache_read_tokens),
            cache_write_tokens: sum(|l| l.cache_write_tokens),
            lines,
        }
    }
}

/// One usage line: tokens, the model they were spent on, and when — never
/// a price. The reader prices it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WireUsageLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<WireModelRef>,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
    /// Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    #[serde(default)]
    pub duration_ms: u64,
}

/// The model a usage line was spent on: chatty-core's `ModelRef`, with the
/// provider as its snake_case name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireModelRef {
    pub provider: String,
    pub model_id: String,
}

/// Progress on an `agent.invoke` (`req.progress`'s `event`): the part of
/// chatty-core's `InvokeAgentProgress` the broker sends a caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireProgress {
    /// The broker admitted the callee's node under this name.
    Admitted(String),
    /// A line about the callee's work.
    Step(String),
    /// The callee's answer, as it streams.
    Text(String),
}

/// Everything a task's terminal status carries beside its state: the
/// `task.run` result's `metadata`, and the `metadata` of an `agent.invoke`
/// result. Every key the worker's mapper writes, plus the evidence a
/// virtual agent's runner adds; anything else does not decode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskMetadata {
    /// What the task spent, its own calls included (ADR-0011, AGE-682).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<WireUsage>,
    /// The compacted tool-call trace (AGE-467), when the task made any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    /// The captured conversation (RC-0, AGE-649), when the task asked for
    /// it and it fit. Opaque: chatty-core's messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<Opaque>,
    /// The byte count of a captured conversation over the cap, sent instead
    /// of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_too_large: Option<u64>,
    /// The valid handoff (TD-2, AGE-693). Opaque: it matches the role's
    /// schema, which only chatty-core reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Opaque>,
    /// The role and errors of a handoff that did not match its schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_invalid: Option<HandoffInvalid>,
    /// How many of the task's answers failed their handoff schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_invalid_count: Option<u32>,
    /// The runner's evidence envelope (ADR-0011 C12, AGE-406), added by the
    /// broker for a virtual agent's worker. Opaque: the runner's facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Opaque>,
}

impl TaskMetadata {
    /// Whether it carries nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A handoff that failed its schema: the role and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffInvalid {
    pub role: String,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_has_the_shape_the_worker_writes() {
        let usage = WireUsage::from_lines(vec![
            WireUsageLine {
                model: Some(WireModelRef {
                    provider: "open_router".into(),
                    model_id: "kit/coder".into(),
                }),
                input_tokens: 20,
                output_tokens: 3,
                at: Some(1_700_000_000_000),
                duration_ms: 12,
                ..WireUsageLine::default()
            },
            WireUsageLine {
                input_tokens: 5,
                ..WireUsageLine::default()
            },
        ]);
        assert_eq!(
            serde_json::to_value(&usage).unwrap(),
            json!({
                "inputTokens": 25, "outputTokens": 3, "cacheReadTokens": 0, "cacheWriteTokens": 0,
                "lines": [
                    {"model": {"provider": "open_router", "model_id": "kit/coder"},
                     "inputTokens": 20, "outputTokens": 3, "cacheReadTokens": 0,
                     "cacheWriteTokens": 0, "at": 1_700_000_000_000u64, "durationMs": 12},
                    {"inputTokens": 5, "outputTokens": 0, "cacheReadTokens": 0,
                     "cacheWriteTokens": 0, "durationMs": 0},
                ]
            })
        );
    }

    #[test]
    fn progress_keeps_its_externally_tagged_shape() {
        assert_eq!(
            serde_json::to_value(WireProgress::Text("hi".into())).unwrap(),
            json!({"Text": "hi"})
        );
        assert_eq!(
            serde_json::from_str::<WireProgress>(r#"{"Step":"read_file"}"#).unwrap(),
            WireProgress::Step("read_file".into())
        );
        assert!(serde_json::from_str::<WireProgress>(r#"{"Finished":{}}"#).is_err());
    }
}
