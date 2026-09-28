//! Who may call whom (PL-S2 DP-1, PL-D4): delegation rights come from the
//! agents' specs, not from their tool profiles.
//!
//! A call is allowed when both specs allow it: the caller's
//! `swarm.delegates_to` lists the callee (by name or glob), the callee is
//! `swarm.exposed`, and the callee's `swarm.callers`, when set, lists the
//! caller. [`may_call`] is that rule and nothing else — pure, so the broker
//! can ask it before anything is spawned.
//!
//! The other [`Refusal`]s are the call chain's and the budget's (DP-2, DP-3):
//! they are defined here so every refusal a delegation can meet is one type.

use crate::agent_spec::AgentSpec;

/// The calling side of a delegation, as the broker knows it.
#[derive(Clone, Copy, Debug)]
pub struct CallerView<'a> {
    pub spec: &'a AgentSpec,
}

/// Why a delegation is refused before it starts.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The caller's `delegates_to` has no entry matching the callee.
    NotListed { caller: String, callee: String },
    /// The callee's spec sets `exposed = false`.
    NotExposed { callee: String },
    /// The callee's `callers` is set and does not match the caller.
    CallerNotAllowed { caller: String, callee: String },
    /// The callee is already on the call chain.
    Cycle { chain: Vec<String>, callee: String },
    /// The call would be deeper than the chain allows.
    TooDeep { depth: u8, max: u8 },
    /// The chain's remaining budget is spent: `turns`, `seconds` or `usd`.
    BudgetSpent { what: &'static str },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotListed { caller, callee } => {
                write!(f, "not_listed: {caller} may not call {callee}")
            }
            Self::NotExposed { callee } => write!(f, "not_exposed: {callee} is not exposed"),
            Self::CallerNotAllowed { caller, callee } => {
                write!(
                    f,
                    "caller_not_allowed: {callee} does not accept calls from {caller}"
                )
            }
            Self::Cycle { chain, callee } => {
                write!(f, "cycle: {} → {callee}", chain.join(" → "))
            }
            Self::TooDeep { depth, max } => write!(f, "too_deep: depth {depth} > max {max}"),
            Self::BudgetSpent { what } => write!(f, "budget_spent: {what}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether `caller` may call `callee`, by their specs alone.
pub fn may_call(caller: CallerView, callee: &AgentSpec) -> Result<(), Refusal> {
    let caller_name = &caller.spec.agent.name;
    let callee_name = &callee.agent.name;
    if !matches_any(&caller.spec.swarm.delegates_to, callee_name) {
        return Err(Refusal::NotListed {
            caller: caller_name.clone(),
            callee: callee_name.clone(),
        });
    }
    if !callee.swarm.exposed {
        return Err(Refusal::NotExposed {
            callee: callee_name.clone(),
        });
    }
    if let Some(callers) = &callee.swarm.callers
        && !matches_any(callers, caller_name)
    {
        return Err(Refusal::CallerNotAllowed {
            caller: caller_name.clone(),
            callee: callee_name.clone(),
        });
    }
    Ok(())
}

/// Whether any of `patterns` matches `name`.
fn matches_any(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, name))
}

/// `*` matches any run of characters, including none; every other character
/// matches itself. Spec names hold no other glob syntax worth supporting.
fn glob_match(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let middle: Vec<&str> = parts.collect();
    let Some((last, middle)) = middle.split_last() else {
        // No `*` at all: the whole name must be the pattern.
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_on_star_only() {
        for (pattern, name, expected) in [
            ("local-coder", "local-coder", true),
            ("local-coder", "local-coder-2", false),
            ("local-coder", "local", false),
            ("*", "anything", true),
            ("*", "", true),
            ("*-reviewer", "local-reviewer", true),
            ("*-reviewer", "local-reviewer-x", false),
            ("local-*", "local-coder", true),
            ("local-*", "remote-coder", false),
            ("a*b*c", "abc", true),
            ("a*b*c", "a-b-b-c", true),
            ("a*b*c", "acb", false),
            ("ab*ba", "aba", false),
            ("?", "?", true),
            ("?", "a", false),
        ] {
            assert_eq!(glob_match(pattern, name), expected, "{pattern} vs {name}");
        }
    }

    fn spec(name: &str) -> AgentSpec {
        AgentSpec::named(name)
    }

    /// Invariant 1: every combination of listed / not listed × exposed / not
    /// × `callers` absent / including / excluding. Both specs must allow a
    /// call; the first rule that fails names the refusal, in the order
    /// listed → exposed → callers.
    #[test]
    fn may_call_truth_table() {
        #[derive(Clone, Copy, Debug)]
        enum Callers {
            Absent,
            Including,
            Excluding,
        }
        let mut seen = 0;
        for listed in [true, false] {
            for exposed in [true, false] {
                for callers in [Callers::Absent, Callers::Including, Callers::Excluding] {
                    let mut caller = spec("reviewer");
                    caller.swarm.delegates_to = if listed {
                        vec!["other".to_string(), "local-*".to_string()]
                    } else {
                        vec!["other".to_string()]
                    };
                    let mut callee = spec("local-coder");
                    callee.swarm.exposed = exposed;
                    callee.swarm.callers = match callers {
                        Callers::Absent => None,
                        Callers::Including => Some(vec!["lead".to_string(), "rev*".to_string()]),
                        Callers::Excluding => Some(vec!["lead".to_string()]),
                    };

                    let expected = if !listed {
                        Err(Refusal::NotListed {
                            caller: "reviewer".to_string(),
                            callee: "local-coder".to_string(),
                        })
                    } else if !exposed {
                        Err(Refusal::NotExposed {
                            callee: "local-coder".to_string(),
                        })
                    } else if matches!(callers, Callers::Excluding) {
                        Err(Refusal::CallerNotAllowed {
                            caller: "reviewer".to_string(),
                            callee: "local-coder".to_string(),
                        })
                    } else {
                        Ok(())
                    };
                    assert_eq!(
                        may_call(CallerView { spec: &caller }, &callee),
                        expected,
                        "listed={listed} exposed={exposed} callers={callers:?}"
                    );
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, 12);

        // An empty `delegates_to` lists nobody, whoever the callee is.
        assert!(matches!(
            may_call(
                CallerView {
                    spec: &spec("coordinator")
                },
                &spec("anyone")
            ),
            Err(Refusal::NotListed { .. })
        ));
    }

    #[test]
    fn refusals_render_as_the_model_reads_them() {
        let cases = [
            (
                Refusal::NotListed {
                    caller: "reviewer".to_string(),
                    callee: "data-coder".to_string(),
                },
                "not_listed: reviewer may not call data-coder",
            ),
            (
                Refusal::Cycle {
                    chain: vec!["coordinator".to_string(), "reviewer".to_string()],
                    callee: "coordinator".to_string(),
                },
                "cycle: coordinator → reviewer → coordinator",
            ),
            (
                Refusal::TooDeep { depth: 5, max: 4 },
                "too_deep: depth 5 > max 4",
            ),
            (Refusal::BudgetSpent { what: "usd" }, "budget_spent: usd"),
        ];
        for (refusal, text) in cases {
            assert_eq!(refusal.to_string(), text);
        }
    }
}
