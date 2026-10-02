//! `human.approve`: an execution or write approval, answered only by the
//! root (ADR-0021 § 2, EN-2a).
//!
//! A worker whose command or write needs a human sends one
//! [`ApprovalRequest`] as a `human.approve` request on its own connection
//! and waits for the [`ApprovalVerdict`] as its result. The broker never
//! shows it to the worker's caller: it delivers it straight to the root
//! ([`CallEvent::Approve`](crate::CallEvent::Approve)) under an id of its own,
//! and the root's answer ([`Transport::approve`](crate::Transport::approve))
//! goes back to the request that asked. No intermediate sees it, so none can
//! answer it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which kind of approval: a command (shell, git, browser, plugin) or a
/// filesystem write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Exec,
    Write,
}

/// The agent that asked for an approval or asked a question: its
/// broker-assigned name and the chain of spec names it runs under, root
/// first. The broker's stamp, on both `human.*` methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Asker {
    pub agent: String,
    pub chain: Vec<String>,
}

/// `human.approve`'s params: what the approval is for.
///
/// `asker` is the broker's to set. Whatever a worker puts there is
/// overwritten on the first broker hop with the name and chain its
/// connection was admitted under, so the root's card names the agent that
/// actually asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub kind: ApprovalKind,
    pub command_or_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<Asker>,
}

/// `human.approve`'s result. Anything short of a human's approval — a deny,
/// a timeout, no root to ask, too many approvals pending — is `Denied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalVerdict {
    Approved,
    Denied,
}

impl ApprovalVerdict {
    pub fn approved(self) -> bool {
        self == Self::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_request_and_its_result_have_the_wire_shape() {
        let request = ApprovalRequest {
            kind: ApprovalKind::Write,
            command_or_path: "src/lib.rs".into(),
            diff_stat: Some("+3 -1".into()),
            asker: None,
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"kind": "write", "command_or_path": "src/lib.rs", "diff_stat": "+3 -1"})
        );
        assert_eq!(
            serde_json::to_value(ApprovalVerdict::Approved).unwrap(),
            json!("approved")
        );
        assert_eq!(
            serde_json::from_value::<ApprovalVerdict>(json!("denied")).unwrap(),
            ApprovalVerdict::Denied
        );
    }
}
