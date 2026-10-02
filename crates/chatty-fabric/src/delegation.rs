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
//!
//! A callee runs under the tighter of its own spec's budget and what its
//! caller has left (DP-3, [`CallChain::budget`]): turns, time and dollars.
//! What the caller has left is the chain's own budget for the calling run,
//! narrowed by what the caller says it has spent — a caller can only ever
//! lower it, never raise it.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::directory::ROOT_NAME;

/// The deepest a call may go: the root's callee is depth 1.
pub const MAX_DEPTH: u8 = 4;

/// How long past its deadline a run may go before it is stopped: a long
/// tool call or model call can keep a run from ever seeing its deadline. A
/// tenth of the `budget`, between 5 s and 2 min. A headless run stops its
/// own pass this late; the broker ends a call whose callee is still going
/// this late with a failed result (DP-3).
pub fn deadline_grace(budget: Duration) -> Duration {
    (budget / 10).clamp(Duration::from_secs(5), Duration::from_secs(120))
}

/// What is left of a budget where a call is made (DP-3). `None` is no
/// limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remaining {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
}

impl Remaining {
    /// No limit at all.
    pub fn is_unlimited(&self) -> bool {
        self.turns.is_none() && self.seconds.is_none() && self.usd.is_none()
    }

    /// Each limit the tighter of the two.
    pub fn min(&self, other: &Remaining) -> Remaining {
        fn tighter<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(if b < a { b } else { a }),
                (a, b) => a.or(b),
            }
        }
        Remaining {
            turns: tighter(self.turns, other.turns),
            seconds: tighter(self.seconds, other.seconds),
            usd: tighter(self.usd, other.usd),
        }
    }

    /// The first limit that is used up — `turns`, `seconds` or `usd` — if
    /// any: nothing is left of it to hand a callee.
    pub fn spent(&self) -> Option<&'static str> {
        if self.turns == Some(0) {
            Some("turns")
        } else if self.seconds == Some(0) {
            Some("seconds")
        } else if self.usd.is_some_and(|usd| usd <= 0.0) {
            Some("usd")
        } else {
            None
        }
    }

    /// Refuse with [`Refusal::BudgetSpent`] when a limit is used up.
    pub fn check(&self) -> Result<(), Refusal> {
        match self.spent() {
            Some(what) => Err(Refusal::BudgetSpent {
                what: what.to_string(),
            }),
            None => Ok(()),
        }
    }
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

    /// What this chain leaves at `now`: its turns and dollars, and its
    /// deadline as whole seconds left (rounded down, so a call made in the
    /// last second reads as out of time).
    pub fn left_at(&self, now: SystemTime) -> Remaining {
        Remaining {
            seconds: self
                .deadline
                .map(|d| d.duration_since(now).unwrap_or_default().as_secs()),
            ..self.remaining.clone()
        }
    }

    /// This chain (already [`extend`](Self::extend)ed to the callee) with
    /// the budget the callee runs under (DP-3), or why the call may not be
    /// made: nothing is left.
    ///
    /// What the caller has left is the chain's own budget at `now` narrowed
    /// by `caller_left`, what the caller counts as left of it (its turns
    /// and dollars already spent, delegated usage included). The callee
    /// then gets the tighter of that and `callee_own`, its spec's own
    /// budget: `turns = min(own, left)`, `deadline = min(now + own,
    /// chain.deadline)`, `usd = min(own, left)`.
    pub fn budget(
        mut self,
        caller_left: &Remaining,
        callee_own: &Remaining,
        now: SystemTime,
    ) -> Result<CallChain, Refusal> {
        let left = self.left_at(now).min(caller_left);
        left.check()?;
        let callee = left.min(callee_own);
        self.deadline = callee
            .seconds
            .map(|seconds| now + Duration::from_secs(seconds));
        self.remaining = Remaining {
            seconds: None,
            ..callee
        };
        Ok(self)
    }
}

/// Why a delegation is refused before it starts. Serialises as
/// `{"reason": "cycle", …}`; its display is what the calling model reads
/// after `Error: invoke_agent: `.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
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

/// The spec rules a broker asks before a call (DP-1's `may_call`): may a
/// node started as spec `caller` call spec `callee`, and may the root?
pub trait CallPolicy: Send + Sync {
    fn may_call(&self, caller: &str, callee: &str) -> Result<(), Refusal>;

    /// May the broker's root call spec `callee`? A root that runs as a spec
    /// (a `--team` leader, an `--agent <spec>` root) is checked like any
    /// caller; a plain root, whose tools already decide whether it may
    /// delegate at all, may call anyone (AGE-745).
    fn root_may_call(&self, callee: &str) -> Result<(), Refusal>;

    /// Spec `callee`'s own budget (DP-3): its `max_agent_turns`,
    /// `max_duration` in seconds and `cap_usd`. Unlimited by default.
    fn budget(&self, _callee: &str) -> Remaining {
        Remaining::default()
    }
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

    /// DP-3: the callee gets the tighter of its own budget and what the
    /// caller has left; the caller's count only ever narrows the chain's.
    #[test]
    fn a_callee_runs_under_the_tighter_of_its_own_and_the_callers_budget() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut caller = CallChain::root("t").extend("a").unwrap();
        caller.deadline = Some(now + Duration::from_secs(30));
        caller.remaining = Remaining {
            turns: Some(10),
            seconds: None,
            usd: Some(1.0),
        };
        let callee_own = Remaining {
            turns: Some(50),
            seconds: Some(3_600),
            usd: Some(0.25),
        };
        let caller_left = Remaining {
            turns: Some(2),
            seconds: None,
            usd: Some(5.0),
        };
        let callee = caller
            .extend("b")
            .unwrap()
            .budget(&caller_left, &callee_own, now)
            .unwrap();
        assert_eq!(callee.deadline, Some(now + Duration::from_secs(30)));
        assert_eq!(
            callee.remaining,
            Remaining {
                turns: Some(2),
                seconds: None,
                usd: Some(0.25),
            },
            "a caller claiming more dollars than its chain has gets the chain's"
        );
        assert_eq!(callee.left_at(now).seconds, Some(30));
    }

    #[test]
    fn a_spent_budget_refuses_by_what_is_spent() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let chain = CallChain::root("t").extend("a").unwrap();
        for (left, what) in [
            (
                Remaining {
                    turns: Some(0),
                    ..Remaining::default()
                },
                "turns",
            ),
            (
                Remaining {
                    seconds: Some(0),
                    ..Remaining::default()
                },
                "seconds",
            ),
            (
                Remaining {
                    usd: Some(-0.01),
                    ..Remaining::default()
                },
                "usd",
            ),
        ] {
            assert_eq!(
                chain.clone().budget(&left, &Remaining::default(), now),
                Err(Refusal::BudgetSpent {
                    what: what.to_string()
                })
            );
        }
        let mut late = chain.clone();
        late.deadline = Some(now);
        assert_eq!(
            late.budget(&Remaining::default(), &Remaining::default(), now),
            Err(Refusal::BudgetSpent {
                what: "seconds".to_string()
            }),
            "a chain past its deadline has no time to hand on"
        );
        let unlimited = chain
            .budget(&Remaining::default(), &Remaining::default(), now)
            .unwrap();
        assert_eq!(unlimited.deadline, None);
        assert!(unlimited.remaining.is_unlimited());
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
