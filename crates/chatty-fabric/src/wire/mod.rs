//! The worker socket's types, as they cross it (ADR-0021 § 1, EN-3a).
//!
//! Every line on a worker's socket is a v3 envelope (`chatty-protocol-gateway`'s
//! `FrameCodec` reads and writes it); what rides in its `params`, `result`
//! and `error` is defined here, split by who may send it:
//!
//! - [`worker`]: what a worker sends — [`WorkerRequest`],
//!   [`WorkerNotification`], and its results to the broker's requests
//!   ([`TaskOutcome`] for a `task.run`, an [`AskReply`](crate::AskReply)
//!   for a relayed `human.ask`).
//! - [`broker`]: what the broker sends — [`BrokerRequest`],
//!   [`BrokerNotification`], and its results to the worker's requests
//!   ([`Welcome`], one result type per method, [`WireError`]).
//!
//! Each direction is its own closed set: a method the peer may not send
//! does not decode. Every struct denies unknown fields, nothing is
//! `untagged` or `flatten`ed, and a payload is decoded straight into its
//! typed struct, so a duplicate key is an error rather than a last-wins.
//!
//! The payloads chatty-core owns have wire-owned mirrors in [`payload`]:
//! usage ([`WireUsage`]), call progress ([`WireProgress`]) and a task's
//! terminal metadata ([`TaskMetadata`]); chatty-core converts at its edge.
//! Exactly four payloads are not the wire's to type — the captured
//! conversation, the handoff answer, the handoff schema and a virtual
//! agent's evidence data — and cross as [`Opaque`] values, capped and never
//! read by the broker.

pub mod broker;
mod error;
mod identity;
mod opaque;
pub mod payload;
pub mod schema;
pub mod worker;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use broker::{
    AgentCapabilities, AgentEntry, AgentEntrySkill, BrokerNotification, BrokerRequest,
    ProgressParams, RelayedAskParams, TaskRunParams, Welcome,
};
pub use error::WireError;
pub use identity::TaskIdentity;
pub use opaque::{OPAQUE_CAP_BYTES, Opaque, OpaqueError};
pub use payload::{
    HandoffInvalid, TaskMetadata, WireModelRef, WireProgress, WireUsage, WireUsageLine,
};
pub use worker::{
    HelloParams, ParticipantCard, ParticipantSkill, TaskEvent, TaskOutcome, WorkerNotification,
    WorkerRequest, WorkerSwarmItem,
};

/// The participant protocol's version. Every line carries it as `v`.
pub const PROTOCOL_VERSION: u64 = 3;

/// Why a method's params do not decode. The codec closes the connection on
/// every one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Not the method's params (or params where it takes none).
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// A method this peer may not send.
    #[error("'{0}' is not a method this peer may send")]
    WrongDirection(String),
}

/// `method`'s params, typed.
fn params<T: serde::de::DeserializeOwned>(
    method: &str,
    raw: Option<&serde_json::value::RawValue>,
) -> Result<T, DecodeError> {
    let raw = raw.ok_or_else(|| DecodeError::Malformed(format!("'{method}' needs params")))?;
    serde_json::from_str(raw.get()).map_err(|e| DecodeError::Malformed(format!("{method}: {e}")))
}

/// A method that takes no params has none.
fn no_params(method: &str, raw: Option<&serde_json::value::RawValue>) -> Result<(), DecodeError> {
    match raw {
        None => Ok(()),
        Some(_) => Err(DecodeError::Malformed(format!(
            "'{method}' takes no params"
        ))),
    }
}

/// The state of one task, in A2A's vocabulary.
///
/// A2A's own spelling is kebab-case (`input-required`), and these values are
/// copied verbatim into the `status.state` field the broker serves, so the
/// serde renaming here is part of the public contract rather than a style
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    /// The participant is blocked on a human. On a worker's connection that
    /// is a `human.ask` request, never a status (EN-2b); the state is A2A's,
    /// for an A2A peer's task.
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
        f.write_str(match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input-required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        })
    }
}

/// A request id, for `req.cancel` (both directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdParams {
    pub id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for state in [
            TaskState::Submitted,
            TaskState::Working,
            TaskState::InputRequired,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::Value::String(state.to_string())
            );
        }
        assert_eq!(TaskState::InputRequired.to_string(), "input-required");
    }
}
