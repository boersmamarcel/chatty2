//! Who may call whom (PL-S2 DP-1, PL-D4): delegation rights come from the
//! agents' specs, not from their tool profiles.
//!
//! A call is allowed when both specs allow it: the caller's
//! `swarm.delegates_to` lists the callee (by name or glob), the callee is
//! `swarm.exposed`, and the callee's `swarm.callers`, when set, lists the
//! caller. [`may_call`] is that rule and nothing else — pure, so the broker
//! can ask it before anything is spawned.
//!
//! [`Refusal`] is `chatty_fabric`'s, because it crosses the fabric: the
//! broker refuses a call before anything is spawned — by these rules
//! ([`SpecPolicy`]), by the call chain's cycle and depth (DP-2), or by the
//! budget (DP-3) — and the calling model reads the typed reason wherever
//! the broker runs.

use std::collections::BTreeMap;

use chatty_fabric::{CallPolicy, Refusal, Remaining};

use crate::agent_spec::AgentSpec;

/// The calling side of a delegation, as the broker knows it.
#[derive(Clone, Copy, Debug)]
pub struct CallerView<'a> {
    pub spec: &'a AgentSpec,
}

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

/// [`may_call`] over the specs a broker publishes, which is what the broker
/// asks before a node's `invoke_agent` (DP-2). A name with no spec here —
/// the default `local-agent`, a registered participant — is a bare spec:
/// it lists nobody and is exposed to anyone.
///
/// The root's own calls are checked against the spec it runs as, when it
/// runs as one ([`with_root`](Self::with_root), AGE-745).
#[derive(Clone, Debug, Default)]
pub struct SpecPolicy {
    specs: BTreeMap<String, AgentSpec>,
    root: Option<AgentSpec>,
}

impl SpecPolicy {
    pub fn new(specs: impl IntoIterator<Item = AgentSpec>) -> Self {
        Self {
            specs: specs
                .into_iter()
                .map(|spec| (spec.agent.name.clone(), spec))
                .collect(),
            root: None,
        }
    }

    /// Check the root's calls against `root`, the spec the broker's root
    /// runs as: a `--team` leader, an `--agent <spec>` root. `None` is a
    /// plain root, which may call anyone.
    pub fn with_root(mut self, root: Option<AgentSpec>) -> Self {
        self.root = root;
        self
    }

    /// The policy over the specs a broker publishes as virtual agents.
    pub fn for_agents(agents: &[crate::services::virtual_agents::VirtualAgentSpec]) -> Self {
        Self::new(agents.iter().map(|agent| agent.spec.clone()))
    }

    fn spec(&self, name: &str) -> std::borrow::Cow<'_, AgentSpec> {
        match self.specs.get(name) {
            Some(spec) => std::borrow::Cow::Borrowed(spec),
            None => std::borrow::Cow::Owned(AgentSpec::named(name)),
        }
    }
}

impl CallPolicy for SpecPolicy {
    fn may_call(&self, caller: &str, callee: &str) -> Result<(), Refusal> {
        may_call(
            CallerView {
                spec: &self.spec(caller),
            },
            &self.spec(callee),
        )
    }

    fn root_may_call(&self, callee: &str) -> Result<(), Refusal> {
        match &self.root {
            Some(root) => may_call(CallerView { spec: root }, &self.spec(callee)),
            None => Ok(()),
        }
    }

    /// The callee spec's `[budget]` (DP-3): `max_agent_turns` (`0` is no
    /// cap), `max_duration` and `cap_usd`.
    fn budget(&self, callee: &str) -> Remaining {
        let spec = self.spec(callee);
        Remaining {
            turns: spec.budget.max_agent_turns.filter(|turns| *turns > 0),
            seconds: spec.max_duration().map(|duration| duration.as_secs()),
            usd: spec.budget.cap_usd,
        }
    }
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
            (
                Refusal::BudgetSpent {
                    what: "usd".to_string(),
                },
                "budget_spent: usd",
            ),
        ];
        for (refusal, text) in cases {
            assert_eq!(refusal.to_string(), text);
        }
    }

    /// The broker's view: names resolve to the published specs, and a name
    /// with none is a bare spec.
    #[test]
    fn spec_policy_asks_may_call_by_name() {
        let mut lead = spec("lead");
        lead.swarm.delegates_to = vec!["local-*".to_string()];
        let mut hidden = spec("local-hidden");
        hidden.swarm.exposed = false;
        let policy = SpecPolicy::new([lead, spec("local-coder"), hidden]);

        assert_eq!(policy.may_call("lead", "local-coder"), Ok(()));
        assert_eq!(
            policy.may_call("lead", "local-hidden"),
            Err(Refusal::NotExposed {
                callee: "local-hidden".to_string()
            })
        );
        assert_eq!(
            policy.may_call("local-coder", "lead"),
            Err(Refusal::NotListed {
                caller: "local-coder".to_string(),
                callee: "lead".to_string()
            })
        );
        assert_eq!(
            policy.may_call("lead", "local-unknown"),
            Ok(()),
            "an unpublished callee is exposed to anyone"
        );
    }

    /// DP-3: a callee's own budget is its spec's `[budget]`.
    #[test]
    fn spec_policy_reads_the_callees_own_budget() {
        let mut worker = spec("local-worker");
        worker.budget.max_agent_turns = Some(12);
        worker.budget.max_duration = Some("2m".to_string());
        worker.budget.cap_usd = Some(0.5);
        let mut uncapped = spec("local-uncapped");
        uncapped.budget.max_agent_turns = Some(0);
        let policy = SpecPolicy::new([worker, uncapped]);
        assert_eq!(
            policy.budget("local-worker"),
            Remaining {
                turns: Some(12),
                seconds: Some(120),
                usd: Some(0.5),
            }
        );
        assert!(policy.budget("local-uncapped").is_unlimited());
        assert!(policy.budget("local-unknown").is_unlimited());
    }
}
