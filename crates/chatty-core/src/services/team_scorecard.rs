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
//! - **preamble:** the leader's skill (`SKILL.md`) as the addendum. A team
//!   without one adds none: its playbook is in the leader's preamble, which
//!   tells it how to delegate, and the single agent cannot.
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
use crate::services::handoff::HandoffLedger;
use crate::services::swarm_trace::{SwarmTrace, ToolOutcome, UsageLine};
use crate::services::team::Team;
use crate::settings::models::execution_settings::canonical_tool_group;
use crate::settings::models::models_store::{ModelConfig, resolve_model_query};

/// The team's leader answered without delegating to anyone.
pub const NEVER_DELEGATED: &str = "never_delegated";

/// The leader called a role its playbook does not name.
pub const WRONG_ROLE_CALLED: &str = "wrong_role_called";

/// A role answered `APPROVE` and the runner's check failed.
pub const VERIFICATION_PASSED_WRONG: &str = "verification_passed_wrong";

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
            preamble: team.skill_content.clone(),
            ..AgentSection::default()
        },
        tools: ToolsSection {
            profile: union_profile(&roles),
            disable: disabled_by_all(&roles),
            skills: Vec::new(),
        },
        plugins: union_plugins(&roles),
        swarm: Default::default(),
        budget: summed_budget(team, &roles),
    }
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
#[derive(Debug, Clone, Copy)]
pub enum Arm<'a> {
    /// A team's derived single agent, running as `spec`.
    Single { spec: &'a str },
    /// A team: `id`, the spec its leader (the root) runs as, and its
    /// playbook, the text that names the roles it may call.
    Team {
        id: &'a str,
        leader: &'a str,
        playbook: Option<&'a str>,
    },
}

impl<'a> Arm<'a> {
    /// `team`'s arm. Its playbook is the leader's skill or, for a team
    /// without one, the leader's preamble.
    pub fn team(team: &'a Team) -> Self {
        Self::Team {
            id: &team.id,
            leader: &team.leader.agent.name,
            playbook: team
                .skill_content
                .as_deref()
                .or(team.leader.agent.preamble.as_deref()),
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
#[derive(Debug, Clone, Copy)]
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
/// - [`WRONG_ROLE_CALLED`]: a team's root called a spec its playbook does
///   not name, as a whole word;
/// - [`HANDOFF_MISREAD`]: TD-2's read rules, from the handoff ledger;
/// - [`VERIFICATION_PASSED_WRONG`]: an answer to the root's `invoke_agent`
///   opened with `APPROVE`, and the runner's check failed.
pub fn failure_tags(record: &RunRecord<'_>) -> Vec<String> {
    let tree = record.trace.tree();
    let called: Vec<&str> = tree
        .children(tree.root())
        .iter()
        .map(|id| tree.get(*id).spec.as_str())
        .collect();
    let mut tags = Vec::new();
    if let Arm::Team { playbook, .. } = record.arm {
        if called.is_empty() {
            tags.push(NEVER_DELEGATED.to_string());
        }
        if let Some(playbook) = playbook
            && called.iter().any(|spec| !names_word(playbook, spec))
        {
            tags.push(WRONG_ROLE_CALLED.to_string());
        }
    }
    if let Some(ledger) = record.handoffs {
        tags.extend(ledger.failure_tags());
    }
    if !record.solved && root_heard_approve(record.trace) {
        tags.push(VERIFICATION_PASSED_WRONG.to_string());
    }
    tags
}

/// Whether `text` holds `word` with no name character (`[A-Za-z0-9_-]`) on
/// either side: `local-coder` is not named by `local-coder-2`.
fn names_word(text: &str, word: &str) -> bool {
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_name) && !after.is_some_and(is_name)
    })
}

/// Whether an `invoke_agent` result the root got opens with `APPROVE`.
fn root_heard_approve(trace: &SwarmTrace) -> bool {
    let tree = trace.tree();
    tree.get(tree.root())
        .tool_calls
        .iter()
        .filter(|call| call.name == "invoke_agent")
        .any(|call| match &call.outcome {
            ToolOutcome::Done { result } => opens_with_approve(result),
            _ => false,
        })
}

/// A verdict's first line, past any markdown, is `APPROVE…`. The result is
/// the tool's JSON, whose `response` is the agent's answer, or that answer.
fn opens_with_approve(result: &str) -> bool {
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
            line.trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
                .starts_with("APPROVE")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::Grant;
    use crate::models::token_usage::TokenPricing;
    use crate::services::team::{TeamFile, TeamSource};
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

    #[test]
    fn a_role_is_named_as_a_whole_word() {
        assert!(names_word("delegate to `local-coder`.", "local-coder"));
        assert!(!names_word("use local-coder-2 here", "local-coder"));
        assert!(!names_word("the my-local-coder agent", "local-coder"));
    }

    #[test]
    fn approve_is_the_first_line_of_the_answer() {
        assert!(opens_with_approve("APPROVE\nall good"));
        assert!(opens_with_approve("\n**APPROVE** — tests pass"));
        assert!(opens_with_approve(
            r#"{"response":"APPROVE","success":true}"#
        ));
        assert!(!opens_with_approve("REQUEST_CHANGES\n1. APPROVE later"));
        assert!(!opens_with_approve("I do not approve"));
    }
}
