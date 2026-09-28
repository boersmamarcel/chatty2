//! Who may call whom, and how far a call may reach (PL-S2, fabric spec 3).
//!
//! A [`CallChain`] is the list of spec names a call went through, root
//! first. The broker builds it from its own [`TaskTable`](crate::TaskTable)
//! — the calling run's chain plus the callee — and never takes it from the
//! caller, so a worker cannot claim to be shallower than it is (DP-2).
//! [`CallChain::extend`] refuses a cycle and a call deeper than
//! [`MAX_DEPTH`] before anything is spawned.
//!
//! [`Refusal`] is every reason a delegation can be refused before it starts:
//! the spec rules of [`CallPolicy`] (chatty-core's `delegation_policy`
//! implements them over agent specs), the chain's, and the budget's (DP-3).
//! It crosses the fabric as [`CallError::Delegation`](crate::CallError), so
//! the calling model reads the typed reason wherever the broker is.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::directory::ROOT_NAME;

/// The deepest a call may go: the root's callee is depth 1.
pub const MAX_DEPTH: u8 = 4;

/// What is left of the root's budget where a call is made (DP-3). `None`
/// is no limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Remaining {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
}

/// The chain one run was called through, as the broker stamped it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallChain {
    /// Minted when the root starts a run; every run under it shares it.
    pub root_task_id: String,
    /// Spec names, root first. The root is [`ROOT_NAME`].
    pub chain: Vec<String>,
    /// `chain.len() - 1`: the root is depth 0, its callee depth 1.
    pub depth: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<SystemTime>,
    #[serde(default)]
    pub remaining: Remaining,
}

impl CallChain {
    /// The root's own chain, at depth 0.
    pub fn root(root_task_id: impl Into<String>) -> Self {
        Self {
            root_task_id: root_task_id.into(),
            chain: vec![ROOT_NAME.to_string()],
            depth: 0,
            deadline: None,
            remaining: Remaining::default(),
        }
    }

    /// The chain a call from this run to `callee` (a spec name) runs under,
    /// or why it may not be made: `callee` is already on the chain, or the
    /// call would be deeper than [`MAX_DEPTH`]. The cycle is checked first,
    /// so a loop reads as a loop whatever its depth.
    pub fn extend(&self, callee: &str) -> Result<CallChain, Refusal> {
        if self.chain.iter().any(|name| name == callee) {
            return Err(Refusal::Cycle {
                chain: self.chain.clone(),
                callee: callee.to_string(),
            });
        }
        let depth = self.depth.saturating_add(1);
        if depth > MAX_DEPTH {
            return Err(Refusal::TooDeep {
                depth,
                max: MAX_DEPTH,
            });
        }
        let mut chain = self.chain.clone();
        chain.push(callee.to_string());
        Ok(CallChain {
            root_task_id: self.root_task_id.clone(),
            chain,
            depth,
            deadline: self.deadline,
            remaining: self.remaining.clone(),
        })
    }
}

/// Why a delegation is refused before it starts. Serialises as
/// `{"reason": "cycle", …}`; its display is what the calling model reads
/// after `Error: invoke_agent: `.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    /// The caller's `delegates_to` has no entry matching the callee.
    #[error("not_listed: {caller} may not call {callee}")]
    NotListed { caller: String, callee: String },
    /// The callee's spec sets `exposed = false`.
    #[error("not_exposed: {callee} is not exposed")]
    NotExposed { callee: String },
    /// The callee's `callers` is set and does not match the caller.
    #[error("caller_not_allowed: {callee} does not accept calls from {caller}")]
    CallerNotAllowed { caller: String, callee: String },
    /// The callee is already on the call chain.
    #[error("cycle: {} → {callee}", chain.join(" → "))]
    Cycle { chain: Vec<String>, callee: String },
    /// The call would be deeper than the chain allows.
    #[error("too_deep: depth {depth} > max {max}")]
    TooDeep { depth: u8, max: u8 },
    /// The chain's remaining budget is spent: `turns`, `seconds` or `usd`.
    #[error("budget_spent: {what}")]
    BudgetSpent { what: String },
}

/// The spec rules a broker asks before a node's call (DP-1's `may_call`):
/// may a node started as spec `caller` call spec `callee`?
pub trait CallPolicy: Send + Sync {
    fn may_call(&self, caller: &str, callee: &str) -> Result<(), Refusal>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_grows_by_one_spec_per_call_to_max_depth() {
        let mut chain = CallChain::root("t-1");
        for (depth, name) in ["a", "b", "c", "d"].into_iter().enumerate() {
            chain = chain.extend(name).expect("within the limit");
            assert_eq!(chain.depth as usize, depth + 1);
        }
        assert_eq!(chain.chain, ["root", "a", "b", "c", "d"]);
        assert_eq!(chain.root_task_id, "t-1");
        assert_eq!(
            chain.extend("e"),
            Err(Refusal::TooDeep { depth: 5, max: 4 })
        );
        assert_eq!(
            chain.extend("b"),
            Err(Refusal::Cycle {
                chain: vec![
                    "root".into(),
                    "a".into(),
                    "b".into(),
                    "c".into(),
                    "d".into()
                ],
                callee: "b".into(),
            }),
            "a cycle reads as a cycle even past the limit"
        );
    }

    #[test]
    fn a_refusal_round_trips_tagged_by_reason() {
        let refusal = Refusal::Cycle {
            chain: vec!["root".into(), "a".into()],
            callee: "a".into(),
        };
        let json = serde_json::to_value(&refusal).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"reason": "cycle", "chain": ["root", "a"], "callee": "a"})
        );
        assert_eq!(serde_json::from_value::<Refusal>(json).unwrap(), refusal);
    }
}
