//! The frames a local participant and the broker exchange over the socket.
//!
//! One JSON object per line, in both directions. Newline-delimited JSON is
//! enough because the socket carries one participant and its tasks are
//! multiplexed by `taskId`, so there is no framing problem a length prefix
//! would solve — and a line is greppable in a log, which a length prefix is
//! not.
//!
//! The frames are deliberately *not* A2A: A2A is the broker's public wire
//! format, and a child process is not a public endpoint. The broker maps
//! between the two (see [`super::registry`]), which is the seam that lets a
//! participant stream partial progress without minting JSON-RPC envelopes.
//!
//! # A session
//!
//! ```text
//! participant → {"type":"register","card":{"name":"worker-1",…}}
//! broker      → {"type":"registered","name":"worker-1"}
//! broker      → {"type":"task","taskId":"task-…","text":"summarise foo.rs"}
//! participant → {"type":"status","taskId":"task-…","state":"working","message":"read_file"}
//! participant → {"type":"artifact","taskId":"task-…","text":"foo.rs defines…","lastChunk":false}
//! participant → {"type":"status","taskId":"task-…","state":"completed"}
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The state of one task, in A2A's vocabulary.
///
/// A2A's own spelling is kebab-case (`input-required`), and these values are
/// copied verbatim into the `status.state` field the broker serves, so the
/// serde renaming here is part of the public contract rather than a style
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    /// The participant is blocked on a human. Routing this up the chain to
    /// `ask_user` is AGE-306; the broker forwards the state either way.
    InputRequired,
    Completed,
    Failed,
    Canceled,
}

impl TaskState {
    /// Whether this state ends the task. A terminal status is the last
    /// update a caller sees, and the broker drops the task when it arrives.
    ///
    /// `InputRequired` is *not* terminal: the task is parked, not over.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Via serde so the wire spelling can only be defined once.
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        f.write_str(&s)
    }
}

/// One skill on a participant's agent card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantSkill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// What a participant publishes about itself at registration.
///
/// `name` is the address: callers reach this participant at `/a2a/{name}`,
/// so it has to be unique across the broker and stable for the connection's
/// lifetime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantCard {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub skills: Vec<ParticipantSkill>,
}

/// A frame from a participant to the broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ParticipantFrame {
    /// The first frame on a connection. A second one is a protocol error.
    Register { card: ParticipantCard },
    /// A task moved, optionally with progress text. The broker turns this
    /// into an A2A `TaskStatusUpdateEvent`.
    ///
    /// `metadata` is copied verbatim into the A2A status's `metadata` field.
    /// It is where a turn's token usage rides back: A2A has no usage concept
    /// — usage belongs to the ledger, not to the task protocol — and
    /// inventing a frame for it would put accounting in the wire format.
    /// The broker's ledger (AGE-307) reads it from there.
    #[serde(rename_all = "camelCase")]
    Status {
        task_id: String,
        state: TaskState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    /// A chunk of the task's output, in stream order. The broker turns this
    /// into an A2A `TaskArtifactUpdateEvent`.
    #[serde(rename_all = "camelCase")]
    Artifact {
        task_id: String,
        text: String,
        #[serde(default)]
        last_chunk: bool,
    },
}

/// A frame from the broker to a participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BrokerFrame {
    /// Registration accepted. `name` is what callers address; it is echoed
    /// so a participant that let the broker pick one still learns it.
    Registered { name: String },
    /// Registration refused — a duplicate name, or a card without one. The
    /// broker closes the connection after this frame.
    Rejected { reason: String },
    /// Work. Answer with `Status` / `Artifact` frames carrying this `taskId`
    /// and end with a terminal state.
    #[serde(rename_all = "camelCase")]
    Task { task_id: String, text: String },
    /// The caller went away. Stop working on `taskId`; no reply is required.
    #[serde(rename_all = "camelCase")]
    Cancel { task_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_frames_use_the_documented_wire_names() {
        let frame = ParticipantFrame::Status {
            task_id: "task-1".into(),
            state: TaskState::InputRequired,
            message: Some("which file?".into()),
            metadata: None,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["state"], "input-required");
        assert_eq!(json["message"], "which file?");
        assert!(
            json.get("metadata").is_none(),
            "an absent metadata field stays off the wire"
        );
    }

    #[test]
    fn status_metadata_round_trips() {
        let line = r#"{"type":"status","taskId":"t","state":"completed",
                       "metadata":{"usage":{"inputTokens":12}}}"#;
        let frame: ParticipantFrame = serde_json::from_str(line).unwrap();
        let ParticipantFrame::Status { metadata, .. } = frame else {
            panic!("expected a status frame");
        };
        assert_eq!(metadata.unwrap()["usage"]["inputTokens"], 12);
    }

    #[test]
    fn broker_frames_use_the_documented_wire_names() {
        let json = serde_json::to_value(BrokerFrame::Task {
            task_id: "task-1".into(),
            text: "do it".into(),
        })
        .unwrap();
        assert_eq!(json["type"], "task");
        assert_eq!(json["taskId"], "task-1");
        assert_eq!(json["text"], "do it");
    }

    #[test]
    fn register_frame_round_trips_a_card() {
        let line = r#"{"type":"register","card":{"name":"worker-1","description":"a worker",
                       "skills":[{"name":"edit"}]}}"#;
        let frame: ParticipantFrame = serde_json::from_str(line).unwrap();
        let ParticipantFrame::Register { card } = frame else {
            panic!("expected a register frame");
        };
        assert_eq!(card.name, "worker-1");
        assert_eq!(card.skills[0].name, "edit");
        // Absent optional fields default rather than failing the connection.
        assert_eq!(card.version, "");
        assert!(card.display_name.is_none());
    }

    #[test]
    fn artifact_last_chunk_defaults_to_false() {
        let frame: ParticipantFrame =
            serde_json::from_str(r#"{"type":"artifact","taskId":"t","text":"x"}"#).unwrap();
        let ParticipantFrame::Artifact { last_chunk, .. } = frame else {
            panic!("expected an artifact frame");
        };
        assert!(!last_chunk, "a chunk is only the last one if it says so");
    }

    #[test]
    fn only_completed_failed_and_canceled_end_a_task() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::Submitted.is_terminal());
        assert!(
            !TaskState::InputRequired.is_terminal(),
            "a task waiting on a human is parked, not over (AGE-306)"
        );
    }

    #[test]
    fn task_state_displays_its_wire_spelling() {
        assert_eq!(TaskState::InputRequired.to_string(), "input-required");
        assert_eq!(TaskState::Working.to_string(), "working");
    }
}
