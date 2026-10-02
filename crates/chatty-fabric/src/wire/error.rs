//! The `error` of a v3 error response (ADR-0021 § 1, EN-3a).

use serde::{Deserialize, Serialize};

use crate::{CallError, Refusal};

/// Why a request failed, as the wire carries it: a closed set tagged by
/// `kind`, one variant per [`CallError`] variant plus [`Self::Protocol`],
/// each variant's fields named beside `kind`.
///
/// `message` rides beside them as display text only: it is written from the
/// variant and never read back, so nothing can branch on it. An unknown
/// `kind`, or a field a variant does not have, is a decode error.
///
/// ```text
/// {"kind":"refused","reason":"over the call cap","message":"refused: over the call cap"}
/// {"kind":"delegation","refusal":{"reason":"too_deep","depth":5,"max":4},"message":"too_deep: depth 5 > max 4"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(into = "Repr", from = "Repr")]
pub enum WireError {
    #[error("unknown agent: {agent}")]
    UnknownAgent { agent: String },
    #[error("spawn context refused: {field}: {reason}")]
    SpawnContextRefused { field: String, reason: String },
    #[error("refused: {reason}")]
    Refused { reason: String },
    #[error(transparent)]
    Delegation { refusal: Refusal },
    #[error("cancelled: {reason}")]
    Cancelled { reason: String },
    #[error("disconnected: {reason}")]
    Disconnected { reason: String },
    #[error("{reason}")]
    Failed { reason: String },
    /// The peer broke the protocol: a hello whose schema does not match, a
    /// frame this side cannot accept.
    #[error("protocol: {reason}")]
    Protocol { reason: String },
}

/// [`WireError`] as it is written: the variant's fields and its display.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Repr {
    UnknownAgent {
        agent: String,
        message: String,
    },
    SpawnContextRefused {
        field: String,
        reason: String,
        message: String,
    },
    Refused {
        reason: String,
        message: String,
    },
    Delegation {
        refusal: Refusal,
        message: String,
    },
    Cancelled {
        reason: String,
        message: String,
    },
    Disconnected {
        reason: String,
        message: String,
    },
    Failed {
        reason: String,
        message: String,
    },
    Protocol {
        reason: String,
        message: String,
    },
}

impl From<WireError> for Repr {
    fn from(error: WireError) -> Self {
        let message = error.to_string();
        match error {
            WireError::UnknownAgent { agent } => Self::UnknownAgent { agent, message },
            WireError::SpawnContextRefused { field, reason } => Self::SpawnContextRefused {
                field,
                reason,
                message,
            },
            WireError::Refused { reason } => Self::Refused { reason, message },
            WireError::Delegation { refusal } => Self::Delegation { refusal, message },
            WireError::Cancelled { reason } => Self::Cancelled { reason, message },
            WireError::Disconnected { reason } => Self::Disconnected { reason, message },
            WireError::Failed { reason } => Self::Failed { reason, message },
            WireError::Protocol { reason } => Self::Protocol { reason, message },
        }
    }
}

/// `message` is dropped: it is display text, never data.
impl From<Repr> for WireError {
    fn from(repr: Repr) -> Self {
        match repr {
            Repr::UnknownAgent { agent, .. } => Self::UnknownAgent { agent },
            Repr::SpawnContextRefused { field, reason, .. } => {
                Self::SpawnContextRefused { field, reason }
            }
            Repr::Refused { reason, .. } => Self::Refused { reason },
            Repr::Delegation { refusal, .. } => Self::Delegation { refusal },
            Repr::Cancelled { reason, .. } => Self::Cancelled { reason },
            Repr::Disconnected { reason, .. } => Self::Disconnected { reason },
            Repr::Failed { reason, .. } => Self::Failed { reason },
            Repr::Protocol { reason, .. } => Self::Protocol { reason },
        }
    }
}

impl From<CallError> for WireError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::UnknownAgent(agent) => Self::UnknownAgent { agent },
            CallError::SpawnContextRefused { field, reason } => {
                Self::SpawnContextRefused { field, reason }
            }
            CallError::Refused(reason) => Self::Refused { reason },
            CallError::Delegation(refusal) => Self::Delegation { refusal },
            CallError::Cancelled(reason) => Self::Cancelled { reason },
            CallError::Disconnected(reason) => Self::Disconnected { reason },
            CallError::Failed(reason) => Self::Failed { reason },
            CallError::Protocol(reason) => Self::Protocol { reason },
        }
    }
}

impl From<WireError> for CallError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::UnknownAgent { agent } => Self::UnknownAgent(agent),
            WireError::SpawnContextRefused { field, reason } => {
                Self::SpawnContextRefused { field, reason }
            }
            WireError::Refused { reason } => Self::Refused(reason),
            WireError::Delegation { refusal } => Self::Delegation(refusal),
            WireError::Cancelled { reason } => Self::Cancelled(reason),
            WireError::Disconnected { reason } => Self::Disconnected(reason),
            WireError::Failed { reason } => Self::Failed(reason),
            WireError::Protocol { reason } => Self::Protocol(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn every_call_error() -> Vec<CallError> {
        vec![
            CallError::UnknownAgent("ghost".into()),
            CallError::SpawnContextRefused {
                field: "roster".into(),
                reason: "wider than the caller's".into(),
            },
            CallError::Refused("over the call cap".into()),
            CallError::Delegation(Refusal::TooDeep { depth: 5, max: 4 }),
            CallError::Cancelled("caller went away".into()),
            CallError::Disconnected("socket closed".into()),
            CallError::Failed("boom".into()),
            CallError::Protocol("schema mismatch".into()),
        ]
    }

    #[test]
    fn every_call_error_round_trips_through_the_wire_with_its_display() {
        for error in every_call_error() {
            let wire = WireError::from(error.clone());
            let line = serde_json::to_string(&wire).unwrap();
            let json: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(json["message"], error.to_string(), "{line}");
            let back: WireError = serde_json::from_str(&line).unwrap();
            assert_eq!(CallError::from(back), error, "{line}");
        }
    }

    #[test]
    fn fields_are_named_siblings_of_kind() {
        let refused = WireError::from(CallError::Delegation(Refusal::TooDeep { depth: 5, max: 4 }));
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            json!({"kind": "delegation",
                   "refusal": {"reason": "too_deep", "depth": 5, "max": 4},
                   "message": "too_deep: depth 5 > max 4"})
        );
        assert_eq!(
            serde_json::to_value(WireError::SpawnContextRefused {
                field: "roster".into(),
                reason: "wider".into()
            })
            .unwrap(),
            json!({"kind": "spawn_context_refused", "field": "roster", "reason": "wider",
                   "message": "spawn context refused: roster: wider"})
        );
    }

    #[test]
    fn the_message_is_display_text_and_never_read() {
        let error: WireError = serde_json::from_str(
            r#"{"kind":"refused","reason":"over the cap","message":"anything at all"}"#,
        )
        .unwrap();
        assert_eq!(
            error,
            WireError::Refused {
                reason: "over the cap".into()
            }
        );
        assert_eq!(error.to_string(), "refused: over the cap");
    }

    #[test]
    fn an_unknown_kind_or_field_does_not_decode() {
        for line in [
            r#"{"kind":"teapot","message":"x"}"#,
            r#"{"kind":"refused","reason":"r","message":"m","extra":1}"#,
            r#"{"kind":"refused","message":"no reason"}"#,
            r#"{"kind":"refused","reason":"a","reason":"b","message":"m"}"#,
        ] {
            assert!(serde_json::from_str::<WireError>(line).is_err(), "{line}");
        }
    }
}
