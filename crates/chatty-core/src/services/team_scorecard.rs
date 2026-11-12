//! A team is worth its cost only if it beats one agent with the same tools
//! (TD-1, AGE-692; fabric-team-design §2, §3, invariants 1, 6 and 7).
//!
//! [`derive_single`] builds that one agent from the team itself, so nobody
//! can tune the baseline, or neglect it, until the team wins:
//!
//! - **tools:** the union of the roles' tool profiles (`coder+coordinator`,
//!   [`PROFILE_UNION_SEPARATOR`]; an unprofiled role has every tool, so the
//!   union is unprofiled too) and of their plugins (a plugin two roles load
//!   gets both roles' grants). A tool group is off only if every role has
//!   it off.
//! - **model:** the strongest role's: the highest input plus output price
//!   per million tokens in the price book, ties broken by the smaller name.
//!   A role that names no model, or one the book does not price, never
//!   outranks a priced one. No role names a model: none, as for the team.
//! - **budget:** the roles' turn budgets summed, the leader's being the
//!   team file's `max_agent_turns` when it names one. A role with no cap
//!   (`0` or none) makes the sum `0`, uncapped, and the single agent then
//!   runs under its deadline: the roles' `max_duration`s summed. `cap_usd`
//!   is summed the same way; a role without a cap leaves the sum uncapped.
//! - **preamble:** the team's playbook, wherever it lives: the leader's
//!   preamble, then the leader's skill (`SKILL.md`), under
//!   [`SINGLE_PREFACE`], which tells the agent it works alone and does each
//!   delegated step itself (AGE-1137). The text is not edited: no rule cuts
//!   the delegation out of free prose and leaves a playbook that reads
//!   (Crosscheck's lead would keep "Never change, merge or improve the
//!   answer" and nothing else), and a hand-trimmed copy would be a tuned
//!   baseline. A team with neither adds none.
//! - **skills:** every role's `tools.skills`, each once; **`think` and
//!   `max_tokens`:** the leader's.
//!
//! The single agent delegates to nobody (`swarm.delegates_to` is empty).
//!
//! A benchmark run writes one [`ScorecardRow`] per run, as a JSONL line.
//! Its cost is the run's usage lines, priced on read ([`ScorecardRow::priced`]),
//! never a stored sum, and its failure tags come from the run's
//! [`SwarmTrace`], which holds the edge log's rows ([`failure_tags`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_spec::{AgentSection, AgentSpec, BudgetSection, PluginSpec, ToolsSection};
use crate::factories::PROFILE_UNION_SEPARATOR;
use crate::models::token_usage::{Cost, PriceBook, TokenUsage, price};
use crate::services::delegation_policy::matches_any;
use crate::services::handoff::HandoffLedger;
use crate::services::swarm_trace::{SwarmTrace, ToolOutcome, UsageLine};
use crate::services::team::{Team, TeamVerifier};
use crate::settings::models::execution_settings::canonical_tool_group;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};

/// The team's leader answered without delegating to anyone.
pub const NEVER_DELEGATED: &str = "never_delegated";

/// The leader called a role its `swarm.delegates_to` does not list.
pub const WRONG_ROLE_CALLED: &str = "wrong_role_called";

/// The team's verifier passed the work and the runner's check failed.
pub const VERIFICATION_PASSED_WRONG: &str = "verification_passed_wrong";

/// What heads the single agent's preamble, over the team's playbook.
pub const SINGLE_PREFACE: &str = "You work alone. The instructions below were written for \
the lead of a team of agents, but there is no team: there is no agent to delegate to, and no \
`invoke_agent`, `list_agents` or `best_of` tool. Wherever they tell you to delegate a step or \
to have another agent do it, do that step yourself. Wherever they tell you to do no work \
yourself, ignore that. Skip what only coordinates other agents.";

pub use crate::services::handoff::HANDOFF_MISREAD_TAG as HANDOFF_MISREAD;

/// Team `id`'s derived single agent is named this.
pub fn single_name(team_id: &str) -> String {
    format!("{team_id}-single")
}

/// The single agent with the team's tools, strongest model and summed
/// budget; see the module docs for each rule. `models` resolve the roles'
/// model references, which `book` prices.
pub fn derive_single(team: &Team, models: &[ModelConfig], book: &PriceBook) -> AgentSpec {
    let roles: Vec<&AgentSpec> = std::iter::once(&team.leader).chain(&team.agents).collect();
    AgentSpec {
        agent: AgentSection {
            name: single_name(&team.id),
            description: Some(format!(
                "One agent with team {}'s tools, model and budget: its derived baseline",
                team.id
            )),
            model: strongest_model(&roles, models, book),
            preamble: single_preamble(team),
            think: team.leader.agent.think,
            max_tokens: team.leader.agent.max_tokens,
            ..AgentSection::default()
        },
        tools: ToolsSection {
            profile: union_profile(&roles),
            disable: disabled_by_all(&roles),
            skills: union_skills(&roles),
        },
        plugins: union_plugins(&roles),
        swarm: Default::default(),
        budget: summed_budget(team, &roles),
    }
}

/// [`SINGLE_PREFACE`] over the leader's preamble and the team's skill;
/// `None` when the team has neither.
fn single_preamble(team: &Team) -> Option<String> {
    let playbook: Vec<&str> = [
        team.leader.agent.preamble.as_deref(),
        team.skill_content.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .collect();
    (!playbook.is_empty()).then(|| {
        std::iter::once(SINGLE_PREFACE)
            .chain(playbook)
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

/// Every role's `tools.skills`, each once, in the order the roles name them.
fn union_skills(roles: &[&AgentSpec]) -> Vec<String> {
    let mut skills: Vec<String> = Vec::new();
    for skill in roles.iter().flat_map(|spec| &spec.tools.skills) {
        if !skills.contains(skill) {
            skills.push(skill.clone());
        }
    }
    skills
}

/// The model of the role with the highest price per million tokens.
fn strongest_model(
    roles: &[&AgentSpec],
    models: &[ModelConfig],
    book: &PriceBook,
) -> Option<String> {
    let price_per_mtok = |query: &str| {
        let model = resolve_model_query(models, Some(query))?;
        let pricing = book.pricing(&model.model_ref(), None)?;
        Some(pricing.input_per_million + pricing.output_per_million)
    };
    roles
        .iter()
        .filter_map(|spec| spec.agent.model.as_deref())
        .map(|query| (price_per_mtok(query), query))
        .max_by(|(a_price, a_name), (b_price, b_name)| {
            a_price
                .partial_cmp(b_price)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b_name.cmp(a_name))
        })
        .map(|(_, query)| query.to_string())
}

/// Every role's profile, joined; `None` when a role has none.
fn union_profile(roles: &[&AgentSpec]) -> Option<String> {
    let mut names = BTreeSet::new();
    for spec in roles {
        names.extend(
            spec.tools
                .profile
                .as_deref()?
                .split(PROFILE_UNION_SEPARATOR),
        );
    }
    Some(
        names
            .into_iter()
            .collect::<Vec<_>>()
            .join(&PROFILE_UNION_SEPARATOR.to_string()),
    )
}

/// The tool groups every role has off.
fn disabled_by_all(roles: &[&AgentSpec]) -> Vec<String> {
    let off: Vec<BTreeSet<String>> = roles
        .iter()
        .map(|spec| {
            spec.tools
                .disable
                .iter()
                .map(|g| canonical_tool_group(g))
                .collect()
        })
        .collect();
    let Some((first, rest)) = off.split_first() else {
        return Vec::new();
    };
    first
        .iter()
        .filter(|group| rest.iter().all(|set| set.contains(*group)))
        .cloned()
        .collect()
}

/// Every role's plugins, each module once with every role's grants.
fn union_plugins(roles: &[&AgentSpec]) -> Vec<PluginSpec> {
    let mut plugins: Vec<PluginSpec> = Vec::new();
    for plugin in roles.iter().flat_map(|spec| &spec.plugins) {
        match plugins
            .iter_mut()
            .find(|known| known.module == plugin.module)
        {
            Some(known) => {
                for grant in &plugin.grants {
                    if !known.grants.contains(grant) {
                        known.grants.push(grant.clone());
                    }
                }
            }
            None => plugins.push(plugin.clone()),
        }
    }
    plugins
}

/// The roles' budgets, summed.
fn summed_budget(team: &Team, roles: &[&AgentSpec]) -> BudgetSection {
    let turn_caps: Option<u32> = roles
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let turns = match i {
                0 => team.file.max_agent_turns.or(spec.budget.max_agent_turns),
                _ => spec.budget.max_agent_turns,
            };
            turns.filter(|turns| *turns > 0)
        })
        .sum();
    let durations: Vec<Duration> = roles
        .iter()
        .filter_map(|spec| spec.max_duration())
        .collect();
    BudgetSection {
        max_agent_turns: Some(turn_caps.unwrap_or(0)),
        max_duration: (!durations.is_empty())
            .then(|| format!("{}s", durations.iter().sum::<Duration>().as_secs())),
        cap_usd: roles.iter().map(|spec| spec.budget.cap_usd).sum(),
    }
}

/// Which arm a run was.
#[derive(Debug, Clone)]
pub enum Arm<'a> {
    /// A team's derived single agent, running as `spec`.
    Single { spec: &'a str },
    /// A team: `id`, the spec its leader (the root) runs as, the roles it
    /// may call (its `swarm.delegates_to`, names or `*` globs), and who
    /// passes its work ([`Team::verifiers`]).
    Team {
        id: &'a str,
        leader: &'a str,
        delegates_to: &'a [String],
        verifiers: Vec<TeamVerifier>,
    },
}

impl<'a> Arm<'a> {
    /// `team`'s arm.
    pub fn team(team: &'a Team) -> Self {
        Self::Team {
            id: &team.id,
            leader: &team.leader.agent.name,
            delegates_to: &team.leader.swarm.delegates_to,
            verifiers: team.verifiers(),
        }
    }

    /// The scorecard's `arm`: `single`, or the team's id.
    fn label(&self) -> &str {
        match self {
            Self::Single { .. } => "single",
            Self::Team { id, .. } => id,
        }
    }

    /// The spec the root ran as.
    fn root_role(&self) -> &str {
        match self {
            Self::Single { spec } => spec,
            Self::Team { leader, .. } => leader,
        }
    }
}

/// One benchmark run, finished, as the scorecard reads it.
#[derive(Debug, Clone)]
pub struct RunRecord<'a> {
    pub run: &'a str,
    pub task: &'a str,
    pub arm: Arm<'a>,
    /// The run's swarm: the root's turn and every node under it, the edge
    /// log's rows folded in.
    pub trace: &'a SwarmTrace,
    /// A team's handoff record (TD-2); `None` without `handoffs`.
    pub handoffs: Option<&'a HandoffLedger>,
    /// The runner's check passed.
    pub solved: bool,
    pub wall: Duration,
}

/// One scorecard row, one JSONL line per run (fabric-team-design §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardRow {
    pub run: String,
    /// `single`, or the team's id.
    pub arm: String,
    pub task: String,
    pub solved: bool,
    pub cost: ScorecardCost,
    pub wall_s: u64,
    /// Turns per spec, as the trace counts them; the root under the spec
    /// it ran as.
    pub turns_by_role: BTreeMap<String, u32>,
    /// Invalid handoffs per role (TD-2); empty without `handoffs`.
    pub handoff_invalid_by_role: BTreeMap<String, u32>,
    pub failure_tags: Vec<String>,
}

/// A row's cost: the run's usage lines, facts only. Price them on read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardCost {
    pub lines: Vec<UsageLine>,
}

impl ScorecardRow {
    pub fn new(record: &RunRecord<'_>) -> Self {
        let tree = record.trace.tree();
        let mut turns_by_role = BTreeMap::new();
        for id in tree.preorder() {
            let node = tree.get(id);
            let role = if id == tree.root() {
                record.arm.root_role()
            } else {
                &node.spec
            };
            *turns_by_role.entry(role.to_string()).or_default() += node.turns;
        }
        Self {
            run: record.run.to_string(),
            arm: record.arm.label().to_string(),
            task: record.task.to_string(),
            solved: record.solved,
            cost: ScorecardCost {
                lines: record.trace.total(),
            },
            wall_s: record.wall.as_secs(),
            turns_by_role,
            handoff_invalid_by_role: record
                .handoffs
                .map(HandoffLedger::invalid_by_role)
                .unwrap_or_default(),
            failure_tags: failure_tags(record),
        }
    }

    /// What the row's lines cost at `book`'s prices (AGE-682).
    pub fn priced(&self, book: &PriceBook) -> Cost {
        let lines: Vec<TokenUsage> = self
            .cost
            .lines
            .iter()
            .map(UsageLine::to_token_usage)
            .collect();
        price(&lines, book)
    }
}

/// Why a run went wrong, each tag from one fact of its trace:
///
/// - [`NEVER_DELEGATED`]: a team's root called nobody;
/// - [`WRONG_ROLE_CALLED`]: a team's root called, or tried to call, a spec
///   its leader's `swarm.delegates_to` does not list (AGE-1137);
/// - [`HANDOFF_MISREAD`]: TD-2's read rules, from the handoff ledger;
/// - [`VERIFICATION_PASSED_WRONG`]: one of the team's verifiers answered
///   the root's `invoke_agent` with one of its passing verdicts, and the
///   runner's check failed (AGE-1137).
pub fn failure_tags(record: &RunRecord<'_>) -> Vec<String> {
    let tree = record.trace.tree();
    let children: Vec<&str> = tree
        .children(tree.root())
        .iter()
        .map(|id| tree.get(*id).spec.as_str())
        .collect();
    let invocations = root_invocations(record.trace);
    let mut tags = Vec::new();
    if let Arm::Team { delegates_to, .. } = &record.arm {
        if children.is_empty() {
            tags.push(NEVER_DELEGATED.to_string());
        }
        // A call the broker refused never became a child; its arguments
        // did reach the root.
        if children
            .iter()
            .copied()
            .chain(invocations.iter().map(|(agent, _)| *agent))
            .any(|spec| !matches_any(delegates_to, spec))
        {
            tags.push(WRONG_ROLE_CALLED.to_string());
        }
    }
    if let Some(ledger) = record.handoffs {
        tags.extend(ledger.failure_tags());
    }
    if let Arm::Team { verifiers, .. } = &record.arm
        && !record.solved
        && invocations.iter().any(|(agent, outcome)| match outcome {
            ToolOutcome::Done { result } => verifiers.iter().any(|verifier| {
                verifier.role == *agent && opens_with_verdict(result, &verifier.verdicts)
            }),
            _ => false,
        })
    {
        tags.push(VERIFICATION_PASSED_WRONG.to_string());
    }
    tags
}

/// The root's `invoke_agent` calls: the agent each asked for, by its
/// arguments, and how it ended.
fn root_invocations(trace: &SwarmTrace) -> Vec<(&str, &ToolOutcome)> {
    let tree = trace.tree();
    tree.get(tree.root())
        .tool_calls
        .iter()
        .filter(|call| call.name == "invoke_agent")
        .filter_map(|call| {
            let agent = call.arguments.as_ref()?.get("agent")?.as_str()?;
            Some((agent, &call.outcome))
        })
        .collect()
}

/// A verdict's first line, past any markdown, opens with one of `verdicts`.
/// The result is the tool's JSON, whose `response` is the agent's answer,
/// or that answer.
fn opens_with_verdict(result: &str, verdicts: &[String]) -> bool {
    let parsed = serde_json::from_str::<Value>(result).ok();
    let answer = parsed
        .as_ref()
        .and_then(|value| value.get("response"))
        .and_then(Value::as_str)
        .unwrap_or(result);
    answer
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            let line = line.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
            verdicts
                .iter()
                .any(|verdict| line.starts_with(verdict.as_str()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::Grant;
    use crate::models::token_usage::TokenPricing;
    use crate::services::team::{TeamFile, TeamSource};
    use crate::session::SessionEvent;
    use crate::settings::models::providers_store::ProviderType;

    fn spec(name: &str, profile: Option<&str>) -> AgentSpec {
        let mut spec = AgentSpec::named(name);
        spec.tools.profile = profile.map(str::to_string);
        spec
    }

    fn team(leader: AgentSpec, agents: Vec<AgentSpec>) -> Team {
        Team {
            id: "t".to_string(),
            source: TeamSource::Preset,
            file: TeamFile {
                leader: leader.agent.name.clone(),
                agents: agents.iter().map(|a| a.agent.name.clone()).collect(),
                ..TeamFile::default()
            },
            skill_content: None,
            leader,
            agents,
            handoffs: BTreeMap::new(),
        }
    }

    /// A roster model `id`, priced at `input`/`output` per million tokens.
    fn priced(id: &str, input: f64, output: f64) -> (ModelConfig, TokenPricing) {
        let model = ModelConfig::new(
            id.to_string(),
            id.to_string(),
            ProviderType::OpenRouter,
            format!("vendor/{id}"),
        );
        let pricing = TokenPricing {
            input_per_million: input,
            output_per_million: output,
            ..TokenPricing::default()
        };
        (model, pricing)
    }

    fn roster(models: &[(ModelConfig, TokenPricing)]) -> (Vec<ModelConfig>, PriceBook) {
        let mut book = PriceBook::default();
        for (model, pricing) in models {
            book.insert(model.model_ref(), *pricing);
        }
        (models.iter().map(|(m, _)| m.clone()).collect(), book)
    }

    /// Price decides, the name breaks a tie, and an unpriced or unnamed
    /// model never outranks a priced one.
    #[test]
    fn the_strongest_role_model_is_the_priciest_ties_by_name() {
        let (models, book) = roster(&[
            priced("cheap", 1.0, 2.0),
            priced("dear-b", 3.0, 9.0),
            priced("dear-a", 9.0, 3.0),
        ]);
        let with_model = |name: &str, model: Option<&str>| {
            let mut spec = spec(name, None);
            spec.agent.model = model.map(str::to_string);
            spec
        };
        let derived = |models_by_role: [Option<&str>; 3]| {
            let [lead, a, b] = models_by_role;
            let team = team(
                with_model("lead", lead),
                vec![with_model("a", a), with_model("b", b)],
            );
            derive_single(&team, &models, &book).agent.model
        };
        assert_eq!(
            derived([Some("cheap"), Some("dear-b"), Some("dear-a")]).as_deref(),
            Some("dear-a"),
            "12 per Mtok each: the smaller name wins"
        );
        assert_eq!(
            derived([Some("unknown"), None, Some("cheap")]).as_deref(),
            Some("cheap")
        );
        assert_eq!(derived([None, None, None]), None);
    }

    /// One uncapped role uncaps the sum, which then runs under the summed
    /// deadline; `cap_usd` needs every role capped. The team file's budget
    /// is the leader's.
    #[test]
    fn budgets_sum_and_one_uncapped_role_uncaps_the_single_agent() {
        let budget = |turns: Option<u32>, duration: Option<&str>, usd: Option<f64>| BudgetSection {
            max_agent_turns: turns,
            max_duration: duration.map(str::to_string),
            cap_usd: usd,
        };
        let mut lead = spec("lead", None);
        lead.budget = budget(Some(5), Some("10m"), Some(1.0));
        let mut a = spec("a", None);
        a.budget = budget(Some(7), Some("90s"), Some(0.5));
        let mut capped = team(lead.clone(), vec![a.clone()]);
        capped.file.max_agent_turns = Some(20);
        assert_eq!(
            derive_single(&capped, &[], &PriceBook::default()).budget,
            budget(Some(27), Some("690s"), Some(1.5))
        );

        let mut b = spec("b", None);
        b.budget = budget(Some(0), None, None);
        let uncapped = team(lead, vec![a, b]);
        assert_eq!(
            derive_single(&uncapped, &[], &PriceBook::default()).budget,
            budget(Some(0), Some("690s"), None)
        );
    }

    /// Profiles unite (an unprofiled role has every tool), a group stays off
    /// only when every role has it off, and a shared plugin gets every
    /// role's grants.
    #[test]
    fn tools_and_plugins_unite_and_disabled_groups_intersect() {
        let mut lead = spec("lead", Some("coordinator"));
        lead.tools.disable = vec!["git".into(), "shell_execute".into()];
        let mut a = spec("a", Some("coder+reviewer"));
        a.tools.disable = vec!["shell".into(), "fetch".into()];
        let plugin = |grants: Vec<Grant>| PluginSpec {
            module: "benford".into(),
            grants,
            ..PluginSpec::default()
        };
        lead.plugins = vec![plugin(vec![Grant::Llm])];
        a.plugins = vec![plugin(vec![Grant::Config, Grant::Llm])];

        let single = derive_single(&team(lead.clone(), vec![a]), &[], &PriceBook::default());
        assert_eq!(
            single.tools.profile.as_deref(),
            Some("coder+coordinator+reviewer")
        );
        assert_eq!(single.tools.disable, ["shell"]);
        assert_eq!(single.plugins, [plugin(vec![Grant::Llm, Grant::Config])]);
        assert!(single.swarm.delegates_to.is_empty());
        single
            .validate(None)
            .expect("the derived spec is a valid spec");

        let unprofiled = derive_single(
            &team(lead, vec![spec("b", None)]),
            &[],
            &PriceBook::default(),
        );
        assert_eq!(unprofiled.tools.profile, None);
    }

    /// AGE-1137: the single agent's preamble is the team's playbook,
    /// wherever it lives (the leader's preamble, then the skill), under the
    /// fixed preface that has it do the delegated steps itself; the roles'
    /// skills and the leader's `think` and `max_tokens` carry over.
    #[test]
    fn the_single_preamble_is_the_preface_over_the_whole_playbook() {
        let mut lead = spec("lead", None);
        lead.agent.preamble = Some("Delegate to a, then answer.".into());
        lead.agent.think = Some(false);
        lead.agent.max_tokens = Some(4096);
        lead.tools.skills = vec!["style".into()];
        let mut a = spec("a", None);
        a.tools.skills = vec!["sql".into(), "style".into()];
        a.agent.think = Some(true);
        a.agent.max_tokens = Some(64);
        let mut skilled = team(lead.clone(), vec![a.clone()]);
        skilled.skill_content = Some("# Skill\n\n1. Ask a.\n".into());

        let single = derive_single(&skilled, &[], &PriceBook::default());
        assert_eq!(
            single.agent.preamble.as_deref(),
            Some(
                format!("{SINGLE_PREFACE}\n\nDelegate to a, then answer.\n\n# Skill\n\n1. Ask a.")
                    .as_str()
            )
        );
        assert_eq!(single.tools.skills, ["style", "sql"]);
        assert_eq!(
            (single.agent.think, single.agent.max_tokens),
            (Some(false), Some(4096)),
            "the leader's, not a role's"
        );
        single.validate(None).expect("a valid spec");

        let preamble_only = derive_single(&team(lead, vec![a.clone()]), &[], &PriceBook::default());
        assert_eq!(
            preamble_only.agent.preamble.as_deref(),
            Some(format!("{SINGLE_PREFACE}\n\nDelegate to a, then answer.").as_str()),
            "a team without a skill keeps its lead's playbook"
        );

        let bare = derive_single(
            &team(spec("lead", None), vec![a]),
            &[],
            &PriceBook::default(),
        );
        assert_eq!(bare.agent.preamble, None, "no playbook, no preface");
    }

    /// The root's turn: one `invoke_agent` call per `(agent, outcome)`,
    /// `Ok` its result and `Err` its error (a refused call). The broker's
    /// edge log is left out: no call here became a child.
    fn root_turn(calls: &[(&str, Result<&str, &str>)]) -> SwarmTrace {
        let mut events = vec![SessionEvent::TurnStarted];
        for (n, (agent, outcome)) in calls.iter().enumerate() {
            let id = format!("call-{n}");
            events.push(SessionEvent::ToolCallStarted {
                id: id.clone(),
                name: "invoke_agent".into(),
            });
            events.push(SessionEvent::ToolCallInput {
                id: id.clone(),
                arguments: serde_json::json!({ "agent": agent, "prompt": "go" }).to_string(),
            });
            events.push(match outcome {
                Ok(result) => SessionEvent::ToolCallResult {
                    id,
                    result: serde_json::json!({ "response": result, "agent": agent }).to_string(),
                },
                Err(error) => SessionEvent::ToolCallError {
                    id,
                    error: error.to_string(),
                },
            });
        }
        events.push(SessionEvent::TurnEnded);
        SwarmTrace::from_edges(&[], &events)
    }

    fn tags(arm: Arm<'_>, trace: &SwarmTrace, solved: bool) -> Vec<String> {
        failure_tags(&RunRecord {
            run: "r",
            task: "t",
            arm,
            trace,
            handoffs: None,
            solved,
            wall: Duration::ZERO,
        })
    }

    fn team_arm<'a>(delegates_to: &'a [String], verifiers: &[(&str, &[&str])]) -> Arm<'a> {
        Arm::Team {
            id: "t",
            leader: "lead",
            delegates_to,
            verifiers: verifiers
                .iter()
                .map(|(role, verdicts)| TeamVerifier {
                    role: role.to_string(),
                    verdicts: verdicts.iter().map(|v| v.to_string()).collect(),
                })
                .collect(),
        }
    }

    /// AGE-1137: `wrong_role_called` is a call outside the leader's
    /// `delegates_to`, globs included, whatever its playbook's prose says;
    /// a call the broker refused counts too.
    #[test]
    fn wrong_role_called_is_a_call_off_the_leaders_delegates_to() {
        let delegates_to = vec!["coder".to_string(), "*-reviewer".to_string()];
        let arm = || team_arm(&delegates_to, &[]);
        let listed = root_turn(&[("coder", Ok("done")), ("code-reviewer", Ok("ok"))]);
        assert!(!tags(arm(), &listed, true).contains(&WRONG_ROLE_CALLED.to_string()));

        let refused = root_turn(&[("scout", Err("not_listed: lead may not call scout"))]);
        assert!(tags(arm(), &refused, true).contains(&WRONG_ROLE_CALLED.to_string()));

        // The playbook's prose names the scout only to forbid it; before
        // AGE-1137 that counted as naming it.
        let mut lead = spec("lead", None);
        lead.agent.preamble = Some("Delegate to coder. Never call scout.".into());
        lead.swarm.delegates_to = vec!["coder".into()];
        let forbidden = team(lead.clone(), vec![spec("coder", None), spec("scout", None)]);
        let scouted = root_turn(&[("scout", Ok("found it"))]);
        assert!(
            tags(Arm::team(&forbidden), &scouted, true).contains(&WRONG_ROLE_CALLED.to_string()),
            "named in the prose, not in delegates_to"
        );
        lead.swarm.delegates_to = vec!["coder".into(), "sc*".into()];
        let listed = team(lead, vec![spec("coder", None), spec("scout", None)]);
        assert!(
            !tags(Arm::team(&listed), &scouted, true).contains(&WRONG_ROLE_CALLED.to_string()),
            "listed in delegates_to, whatever the prose says"
        );
    }

    /// AGE-1137: `verification_passed_wrong` is the team's verifier
    /// answering one of its verdicts, never another role's `APPROVE`.
    #[test]
    fn verification_passed_wrong_is_the_verifiers_verdict() {
        let delegates_to = vec!["*".to_string()];
        let verifier: &[(&str, &[&str])] = &[("reviewer", &["APPROVE", "PASS"])];
        let failed = |calls: &[(&str, Result<&str, &str>)], verifiers: &[(&str, &[&str])]| {
            tags(team_arm(&delegates_to, verifiers), &root_turn(calls), false)
                .contains(&VERIFICATION_PASSED_WRONG.to_string())
        };
        assert!(failed(&[("reviewer", Ok("APPROVE\nall good"))], verifier));
        assert!(failed(&[("reviewer", Ok("**PASS**: 12 tests"))], verifier));
        assert!(
            !failed(&[("judge", Ok("APPROVE"))], verifier),
            "not the verifier"
        );
        assert!(!failed(&[("reviewer", Ok("REQUEST_CHANGES"))], verifier));
        assert!(!failed(&[("reviewer", Ok("APPROVE"))], &[]), "no verifier");
        assert!(failed(
            &[("judge", Ok("CHOICE 2: the plan attempt"))],
            &[("judge", &["CHOICE"])]
        ));
        let solved = tags(
            team_arm(&delegates_to, verifier),
            &root_turn(&[("reviewer", Ok("APPROVE"))]),
            true,
        );
        assert!(
            !solved.contains(&VERIFICATION_PASSED_WRONG.to_string()),
            "a pass the check agrees with: {solved:?}"
        );
    }

    #[test]
    fn a_verdict_is_the_first_line_of_the_answer() {
        let approve = ["APPROVE".to_string(), "PASS".to_string()];
        assert!(opens_with_verdict("APPROVE\nall good", &approve));
        assert!(opens_with_verdict("\n**APPROVE** — tests pass", &approve));
        assert!(opens_with_verdict("PASS\n12 passed", &approve));
        assert!(opens_with_verdict(
            r#"{"response":"APPROVE","success":true}"#,
            &approve
        ));
        assert!(!opens_with_verdict(
            "REQUEST_CHANGES\n1. APPROVE later",
            &approve
        ));
        assert!(!opens_with_verdict("I do not approve", &approve));
    }
}
