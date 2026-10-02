//! `human.ask`: a clarifying question, relayed up the caller chain to a
//! human (ADR-0021 § 2, EN-2b).
//!
//! A worker whose `ask_user` waits on someone sends one [`AskRequest`] as a
//! `human.ask` request on its own connection, and parks its task on it
//! until the answers come back as the request's result. The broker stamps
//! who asked ([`Asker`]) on that first hop, then relays the request, first
//! stamp intact, to the asker's caller: a worker gets it as a broker→worker
//! `human.ask` and answers it, or answers [`AskReply::Escalate`], which
//! sends the same request on to the next caller up; the root gets it as
//! [`CallEvent::Ask`](crate::CallEvent::Ask) and answers with
//! [`Transport::answer`](crate::Transport::answer). So whoever answers sees
//! the agent that asked, not the nearest relayer.

use serde::{Deserialize, Serialize};

use crate::{AgentOrigin, Asker};

/// One clarifying question with its pre-made answers: field for field
/// chatty-core's `ClarifyingQuestion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// The answer to one [`Question`]: field for field chatty-core's
/// `ClarificationAnswer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub id: String,
    pub answer: String,
    #[serde(default)]
    pub custom: bool,
}

/// The third-party peer a question came from, when the asking worker only
/// relays it: the A2A agent its `invoke_agent` called, by the name it is
/// configured under, and where that agent runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionOrigin {
    pub agent: String,
    pub origin: AgentOrigin,
}

/// `human.ask`'s params: what is asked, and who asked it.
///
/// `asker` is the broker's to set: whatever a worker puts there is
/// overwritten on the first broker hop with the name and chain its
/// connection was admitted under, and no hop above changes it. `origin` is
/// set by the worker's client code when it relays a third-party peer's
/// question, never from an `ask_user` call's arguments; a question the
/// worker's own model asked has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskRequest {
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<Asker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<QuestionOrigin>,
}

/// A caller's result to a `human.ask` the broker relayed to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskReply {
    /// The answers, for the worker that asked.
    Answers(Vec<Answer>),
    /// This caller cannot answer: the broker forwards the original request,
    /// first stamp intact, to its caller.
    Escalate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_and_its_replies_have_the_wire_shape() {
        let request = AskRequest {
            questions: vec![Question {
                id: "q1".into(),
                question: "Which database?".into(),
                options: vec!["SQLite".into()],
            }],
            asker: None,
            origin: Some(QuestionOrigin {
                agent: "voucher".into(),
                origin: AgentOrigin::RemoteConfigured,
            }),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({
                "questions": [{"id": "q1", "question": "Which database?", "options": ["SQLite"]}],
                "origin": {"agent": "voucher", "origin": "remote_configured"},
            })
        );
        assert_eq!(
            serde_json::to_value(AskReply::Escalate).unwrap(),
            json!("escalate")
        );
        assert_eq!(
            serde_json::from_value::<AskReply>(
                json!({"answers": [{"id": "q1", "answer": "SQLite"}]})
            )
            .unwrap(),
            AskReply::Answers(vec![Answer {
                id: "q1".into(),
                answer: "SQLite".into(),
                custom: false,
            }])
        );
    }
}
