//! One ATIF document for a whole run (TB-2, AGE-664; full run AGE-859).
//!
//! Every step of every agent is in one trajectory. A step names the agent
//! that took it by its path in the tree (`steps[].extra.agent`, e.g.
//! `root/coder-1`) and, for anyone but the root, the delegation step that
//! started that agent (`steps[].extra.parent_step`). An agent's steps come
//! right after the step that delegated to it, so the document reads in the
//! order the work happened along each branch. A plugin tool's call names
//! its plugin in `tool_calls[].extra.plugin` (AGE-5). The agents
//! themselves — their place in the tree, model, own spend and final
//! status — are the roster in `extra.swarm`.
//!
//! Each worker's steps are its captured conversation (RC-0): model turns,
//! reasoning, tool calls and their results. A conversation that was over
//! the capture cap is replaced by a step that says so (`extra.cut_bytes`)
//! followed by the worker's tool calls from the swarm trace; a worker that
//! captured nothing (a stopped one) gets its tool calls, or a step that
//! says nothing was captured. A worker's own usage is the metrics of its
//! last step, so step usage sums to `final_metrics` (AGE-859).
//!
//! [`run_to_atif`] writes a persisted conversation (`exporters::export_run`,
//! which the CLI and the desktop share); [`export_swarm`] writes a live
//! [`SwarmTrace`]; [`swarm_tree_from_atif`] reads a document back into the
//! tree it was written from. Usage is facts (tokens per model); a worker's
//! is never priced here: cost is computed on read (AGE-682).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, ToolResult, ToolResultContent, UserContent};

use super::steps::plugin_extra;
use super::{SCHEMA_VERSION, root_conversation};
use crate::exporters::run_export::ExportRun;
use crate::exporters::types::*;
use crate::services::swarm_trace::{
    AgentNode, AgentRecord, NodeId, NodeStatus, SwarmTrace, ToolCall, ToolOutcome, Tree, UsageLine,
    plugin_of,
};
use chatty_fabric::{CapturedConversation, ROOT_NAME};

/// The tool calls that start other agents: a step holding one is a
/// delegation step, and must have a child trajectory.
const DELEGATING_TOOLS: [&str; 2] = ["invoke_agent", "best_of"];

/// `trace` as one ATIF document. Usage is written as facts (tokens per
/// model) and never priced here: cost is computed on read (AGE-682).
pub fn export_swarm(trace: &SwarmTrace) -> Result<serde_json::Value> {
    serde_json::to_value(swarm_to_atif(trace))
        .context("Failed to serialize the swarm's ATIF export")
}

/// [`export_swarm`], typed.
pub fn swarm_to_atif(trace: &SwarmTrace) -> AtifExport {
    let records = trace.records();
    let mut agents: Vec<Agent> = Vec::with_capacity(records.len());
    for record in records {
        let parent = record.parent;
        agents.push(Agent::from_record(record, parent, &agents));
    }
    // The root has no captured conversation: its steps are its tool calls.
    if let Some(root) = agents.first_mut() {
        root.steps = Some(root.tool_steps());
    }

    let total = trace.total();
    let sum = |f: fn(&UsageLine) -> u32| total.iter().map(f).fold(0u32, u32::saturating_add);
    let (cache_read, cache_write) = (sum(|l| l.cache_read_tokens), sum(|l| l.cache_write_tokens));
    let tree = trace.tree();
    let root = tree.get(tree.root());
    let session_id = match trace.root_task_ids().as_slice() {
        [] => root.name.clone(),
        ids => ids.join(","),
    };
    let final_metrics = AtifFinalMetrics {
        total_prompt_tokens: Some(
            sum(|l| l.input_tokens)
                .saturating_add(cache_read)
                .saturating_add(cache_write),
        ),
        total_completion_tokens: Some(sum(|l| l.output_tokens)),
        total_cost_usd: None,
        total_steps: None,
        extra: AtifFinalMetricsExtra::from_totals(cache_read, cache_write),
    };
    assemble(
        session_id,
        AtifAgent {
            name: "chatty".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            model_name: root.model.as_ref().map(|m| m.model_id.clone()),
            extra: None,
        },
        agents,
        final_metrics,
        AtifExtra {
            feedback: Vec::new(),
            regenerations: Vec::new(),
            swarm: None,
            incomplete: Vec::new(),
        },
    )
}

/// `run` as one ATIF document: the root conversation's steps, and every
/// agent each turn delegated to nested under the step that started it.
pub fn run_to_atif(run: &ExportRun<'_>) -> Result<AtifExport> {
    let root = root_conversation(run.root, run.model_config)?;
    let usage = &root.token_usage;

    let root_node = AgentNode {
        turns: root.assistant_turns,
        usage: root.own_usage.clone(),
        model: root.own_usage.iter().find_map(|line| line.model.clone()),
        status: NodeStatus::Completed,
        ..AgentNode::new(ROOT_NAME, ROOT_NAME, None)
    };
    let mut agents = vec![Agent::from_record(
        AgentRecord {
            node: root_node,
            parent: None,
            started_by: None,
            conversation: None,
        },
        None,
        &[],
    )];
    agents[0].steps = Some(root.steps);
    // Each turn's agents join the one roster under the one root; a root
    // callee hangs off the step of the turn that delegated to it.
    for (step, records) in root.turns {
        let mut index: Vec<usize> = vec![0];
        for record in records.into_iter().skip(1) {
            let parent = record.parent.map(|p| index.get(p).copied().unwrap_or(0));
            let mut agent = Agent::from_record(record, parent, &agents);
            if parent == Some(0) {
                agent.turn_step = Some(step);
            }
            index.push(agents.len());
            agents.push(agent);
        }
    }

    let final_metrics = AtifFinalMetrics {
        total_prompt_tokens: Some(
            usage.total_input_tokens
                + usage.total_cache_read_tokens
                + usage.total_cache_write_tokens,
        ),
        total_completion_tokens: Some(usage.total_output_tokens),
        total_cost_usd: Some(usage.total_estimated_cost_usd),
        total_steps: None,
        extra: AtifFinalMetricsExtra::from_totals(
            usage.total_cache_read_tokens,
            usage.total_cache_write_tokens,
        ),
    };
    Ok(assemble(
        run.root.id.clone(),
        root.agent,
        agents,
        final_metrics,
        root.extra,
    ))
}

/// A step before it is placed, with the calls it holds, so a child agent
/// can be hung off the step that delegated to it.
pub(super) struct Built {
    step: AtifStep,
    calls: Vec<CallRef>,
}

impl Built {
    pub(super) fn new(step: AtifStep, calls: Vec<CallRef>) -> Self {
        Self { step, calls }
    }
}

/// One tool call of a [`Built`] step: every id it is known by (rig's and
/// the provider's), its tool, and the agent it asked for, if it delegated.
pub(super) struct CallRef {
    ids: Vec<String>,
    name: String,
    agent: Option<String>,
}

/// The calls of an assistant message's `content`.
pub(super) fn call_refs(content: &[AssistantContent]) -> Vec<CallRef> {
    content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::ToolCall(tc) => Some(CallRef {
                ids: std::iter::once(tc.id.to_string())
                    .chain(tc.provider.as_ref().map(|p| p.call_id.clone()))
                    .collect(),
                name: tc.function.name.clone(),
                agent: tc
                    .function
                    .arguments
                    .get("agent")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            _ => None,
        })
        .collect()
}

/// One agent of the run while the document is assembled.
struct Agent {
    roster: AtifSwarmAgent,
    parent: Option<usize>,
    started_by: Option<String>,
    /// For a root callee read from a conversation: the root step of the
    /// turn that delegated to it.
    turn_step: Option<usize>,
    /// Its steps, once read: the root's are given, a worker's come from
    /// its conversation or its tool calls.
    steps: Option<Vec<Built>>,
    conversation: Option<CapturedConversation>,
    tool_calls: Vec<ToolCall>,
}

impl Agent {
    fn from_record(record: AgentRecord, parent: Option<usize>, before: &[Agent]) -> Self {
        let node = record.node;
        let path = match parent.and_then(|p| before.get(p)) {
            Some(parent) => format!("{}/{}", parent.roster.path, node.name),
            None => node.name.clone(),
        };
        Self {
            roster: AtifSwarmAgent {
                name: node.name,
                path,
                spec: node.spec,
                parent,
                root_task_id: node.root_task_id,
                model: node.model,
                turns: node.turns,
                text_bytes: node.text_bytes,
                usage: node.usage,
                status: node.status,
            },
            parent,
            started_by: record.started_by,
            turn_step: None,
            steps: None,
            conversation: record.conversation,
            tool_calls: node.tool_calls,
        }
    }

    /// Its steps, read from its conversation, or from its tool calls when
    /// none was captured, and never empty. The reasons the steps are not
    /// its whole run go to `incomplete`.
    fn read_steps(&mut self, incomplete: &mut Vec<String>) -> Vec<Built> {
        let mut steps = match self.steps.take() {
            Some(steps) => steps,
            None => match &self.conversation {
                Some(CapturedConversation::Messages { messages }) => {
                    match serde_json::from_value::<Vec<Message>>(messages.clone()) {
                        Ok(messages) => conversation_steps(&messages),
                        Err(e) => {
                            incomplete.push(format!(
                                "{}: its conversation could not be read ({e}); tool calls only",
                                self.roster.path
                            ));
                            self.tool_steps()
                        }
                    }
                }
                Some(CapturedConversation::TooLarge { bytes }) => {
                    incomplete.push(format!(
                        "{}: its conversation was {bytes} bytes, over the capture cap; \
                         tool calls only",
                        self.roster.path
                    ));
                    let mut cut = Built::new(
                        system_step(format!(
                            "[conversation cut: {bytes} bytes, over the capture cap; \
                             tool calls only from here]"
                        )),
                        Vec::new(),
                    );
                    cut.step.extra = Some(step_extra(String::new(), None));
                    if let Some(extra) = cut.step.extra.as_mut() {
                        extra.cut_bytes = Some(*bytes);
                    }
                    std::iter::once(cut).chain(self.tool_steps()).collect()
                }
                None => {
                    incomplete.push(format!(
                        "{}: no conversation was captured ({}); tool calls only",
                        self.roster.path,
                        status_word(&self.roster.status)
                    ));
                    self.tool_steps()
                }
            },
        };
        // A worker always has a trajectory, if only a step saying nothing
        // was recorded; the root's is its conversation, empty or not.
        if steps.is_empty() && self.parent.is_some() {
            steps.push(Built::new(
                system_step(format!(
                    "[{} {}; nothing it did was recorded]",
                    self.roster.name,
                    status_word(&self.roster.status)
                )),
                Vec::new(),
            ));
        }
        steps
    }

    fn tool_steps(&self) -> Vec<Built> {
        self.tool_calls.iter().map(tool_step).collect()
    }
}

/// Place every agent's steps, number them, and check the result.
fn assemble(
    session_id: String,
    agent: AtifAgent,
    mut agents: Vec<Agent>,
    mut final_metrics: AtifFinalMetrics,
    mut extra: AtifExtra,
) -> AtifExport {
    let mut incomplete = Vec::new();
    let mut steps: Vec<AtifStep> = Vec::new();
    if !agents.is_empty() {
        let mut read: Vec<Option<Vec<Built>>> = agents
            .iter_mut()
            .map(|agent| Some(agent.read_steps(&mut incomplete)))
            .collect();
        // A worker's own spend is its last step's metrics.
        for (agent, built) in agents.iter().zip(read.iter_mut()).skip(1) {
            if let Some(last) = built.as_mut().and_then(|b| b.last_mut())
                && let Some(metrics) = usage_metrics(&agent.roster.usage)
            {
                last.step.metrics = Some(metrics);
            }
        }
        let placed = place_children(&agents, &read);
        emit(0, None, &agents, &mut read, &placed, &mut steps);
    }

    for step in steps.iter_mut() {
        if let Some(best_of) = best_of_extra(step)
            && let Some(extra) = step.extra.as_mut()
        {
            extra.best_of = Some(best_of);
        }
    }
    name_the_judges(&mut steps, &agents);
    check(&steps, &final_metrics, &mut incomplete);

    final_metrics.total_steps = Some(steps.len() as u32);
    extra.swarm = Some(agents.into_iter().map(|agent| agent.roster).collect());
    extra.incomplete = incomplete;
    AtifExport {
        schema_version: SCHEMA_VERSION.to_string(),
        session_id,
        agent,
        steps,
        final_metrics: Some(final_metrics),
        extra: Some(extra),
    }
}

/// For each agent, the index of the step of its parent's that started it.
fn place_children(agents: &[Agent], read: &[Option<Vec<Built>>]) -> Vec<Option<usize>> {
    let mut placed = vec![None; agents.len()];
    // How many of a parent's delegations to each spec are already taken.
    let mut taken: BTreeMap<(usize, String), usize> = BTreeMap::new();
    for (i, agent) in agents.iter().enumerate() {
        let Some(parent) = agent.parent else { continue };
        let Some(steps) = read[parent].as_ref() else {
            continue;
        };
        let holds = |id: &str| {
            steps
                .iter()
                .position(|b| b.calls.iter().any(|c| c.ids.iter().any(|i| i == id)))
        };
        let by_call = agent.started_by.as_deref().and_then(holds);
        let by_spec = || {
            let n = taken
                .entry((parent, agent.roster.spec.clone()))
                .or_default();
            let found = steps
                .iter()
                .enumerate()
                .flat_map(|(s, b)| b.calls.iter().map(move |c| (s, c)))
                .filter(|(_, c)| {
                    c.name == "invoke_agent"
                        && c.agent.as_deref() == Some(agent.roster.spec.as_str())
                })
                .nth(*n)
                .map(|(s, _)| s);
            *n += 1;
            found
        };
        let by_best_of = || {
            steps
                .iter()
                .position(|b| b.calls.iter().any(|c| c.name == "best_of"))
        };
        placed[i] = by_call
            .or(agent.turn_step)
            .or_else(by_spec)
            .or_else(by_best_of)
            .or(Some(steps.len().saturating_sub(1)));
    }
    placed
}

/// Write agent `i`'s steps, each followed by the agents it started.
fn emit(
    i: usize,
    parent_step: Option<u32>,
    agents: &[Agent],
    read: &mut [Option<Vec<Built>>],
    placed: &[Option<usize>],
    steps: &mut Vec<AtifStep>,
) {
    let Some(built) = read[i].take() else { return };
    let path = agents[i].roster.path.clone();
    for (s, mut b) in built.into_iter().enumerate() {
        let step_id = steps.len() as u32 + 1;
        b.step.step_id = step_id;
        let mut extra = b
            .step
            .extra
            .take()
            .unwrap_or_else(|| step_extra(String::new(), None));
        extra.agent = path.clone();
        extra.parent_step = parent_step;
        b.step.extra = Some(extra);
        steps.push(b.step);
        for child in
            (0..agents.len()).filter(|c| agents[*c].parent == Some(i) && placed[*c] == Some(s))
        {
            emit(child, Some(step_id), agents, read, placed, steps);
        }
    }
}

/// A `best_of` step's choice, from its result: the attempt kept, how, and
/// the judge's reason (AGE-853).
fn best_of_extra(step: &AtifStep) -> Option<serde_json::Value> {
    let call = step
        .tool_calls
        .iter()
        .flatten()
        .find(|c| c.function_name == "best_of")?;
    let result = step
        .observation
        .as_ref()?
        .results
        .iter()
        .find(|r| r.source_call_id.as_deref() == Some(call.tool_call_id.as_str()))?;
    let output: serde_json::Value = serde_json::from_str(result.content.as_deref()?).ok()?;
    let attempts: Vec<serde_json::Value> = output
        .get("attempts")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|a| a.get("agent").cloned()).collect())
        .unwrap_or_default();
    Some(serde_json::json!({
        "chosen": output.get("chosen"),
        "selected_by": output.get("selected_by"),
        "reason": output.get("reason"),
        "attempts": attempts,
    }))
}

/// Name the judge on each `best_of` step: the child it started that is not
/// one of its attempts' solvers.
fn name_the_judges(steps: &mut [AtifStep], agents: &[Agent]) {
    let children: Vec<(u32, String)> = steps
        .iter()
        .filter_map(|s| {
            let extra = s.extra.as_ref()?;
            Some((extra.parent_step?, extra.agent.clone()))
        })
        .collect();
    for step in steps.iter_mut() {
        let id = step.step_id;
        let Some(best_of) = step.extra.as_mut().and_then(|e| e.best_of.as_mut()) else {
            continue;
        };
        let solvers: Vec<String> = best_of["attempts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let judge = children
            .iter()
            .filter(|(parent, _)| *parent == id)
            .filter_map(|(_, path)| agents.iter().find(|a| &a.roster.path == path))
            .find(|a| !solvers.contains(&a.roster.spec))
            .map(|a| a.roster.path.clone());
        if let (Some(judge), Some(object)) = (judge, best_of.as_object_mut()) {
            object.insert("judge".to_string(), serde_json::Value::String(judge));
        }
    }
}

/// The completeness check (AGE-859): step usage sums to `final_metrics`,
/// and every delegation step has a child trajectory.
fn check(steps: &[AtifStep], final_metrics: &AtifFinalMetrics, incomplete: &mut Vec<String>) {
    let (prompt, completion) =
        steps
            .iter()
            .filter_map(|s| s.metrics.as_ref())
            .fold((0u32, 0u32), |(p, c), m| {
                (
                    p.saturating_add(m.prompt_tokens.unwrap_or(0)),
                    c.saturating_add(m.completion_tokens.unwrap_or(0)),
                )
            });
    let (want_prompt, want_completion) = (
        final_metrics.total_prompt_tokens.unwrap_or(0),
        final_metrics.total_completion_tokens.unwrap_or(0),
    );
    if (prompt, completion) != (want_prompt, want_completion) {
        incomplete.push(format!(
            "step usage sums to {prompt} prompt and {completion} completion tokens; \
             final_metrics has {want_prompt} and {want_completion}"
        ));
    }
    for step in steps {
        let delegated = step
            .tool_calls
            .iter()
            .flatten()
            .find(|c| DELEGATING_TOOLS.contains(&c.function_name.as_str()));
        let has_child = steps
            .iter()
            .any(|s| s.extra.as_ref().and_then(|e| e.parent_step) == Some(step.step_id));
        if let Some(call) = delegated
            && !has_child
        {
            incomplete.push(format!(
                "step {} delegated ({}) but has no child trajectory",
                step.step_id, call.function_name
            ));
        }
    }
}

/// A worker's captured conversation as steps: one per message, a tool
/// result joining the step whose call it answers.
fn conversation_steps(messages: &[Message]) -> Vec<Built> {
    let mut built: Vec<Built> = Vec::new();
    for message in messages {
        match message {
            Message::User { content } => {
                let mut text = Vec::new();
                for part in content {
                    match part {
                        UserContent::ToolResult(result) => attach_tool_result(&mut built, result),
                        UserContent::Text(t) => text.push(t.text.clone()),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    let mut step = system_step(text.join("\n"));
                    step.source = "user".to_string();
                    built.push(Built::new(step, Vec::new()));
                }
            }
            Message::Assistant { content, .. } => {
                let text: String = content
                    .iter()
                    .filter_map(|ac| match ac {
                        AssistantContent::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect();
                let reasoning: Vec<String> = content
                    .iter()
                    .filter_map(|ac| match ac {
                        AssistantContent::Reasoning(r) => Some(r.display_text()),
                        _ => None,
                    })
                    .filter(|r| !r.is_empty())
                    .collect();
                let tool_calls: Vec<AtifToolCall> = content
                    .iter()
                    .filter_map(|ac| match ac {
                        AssistantContent::ToolCall(tc) => Some(AtifToolCall {
                            tool_call_id: tc
                                .provider
                                .as_ref()
                                .map(|p| p.call_id.clone())
                                .unwrap_or_else(|| tc.id.to_string()),
                            function_name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                            extra: plugin_extra(&tc.function.name),
                        }),
                        _ => None,
                    })
                    .collect();
                let mut step = system_step(text);
                step.source = "agent".to_string();
                step.reasoning_content = (!reasoning.is_empty()).then(|| reasoning.join("\n\n"));
                step.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
                built.push(Built::new(step, call_refs(content)));
            }
            Message::System { .. } => {}
        }
    }
    built
}

/// Put a tool-result message part on the latest step whose call it answers.
pub(super) fn attach_tool_result(built: &mut [Built], result: &ToolResult) {
    let content = result
        .content
        .iter()
        .map(|c| match c {
            ToolResultContent::Text(t) => t.text.clone(),
            ToolResultContent::Json { value } => value.to_string(),
            ToolResultContent::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    attach_result(built, &result.call, content);
}

/// Put a tool's result on the latest step whose call it answers.
fn attach_result(built: &mut [Built], id: &str, content: String) {
    let Some(b) = built
        .iter_mut()
        .rev()
        .find(|b| b.calls.iter().any(|c| c.ids.iter().any(|i| i == id)))
    else {
        return;
    };
    let call_id = b
        .step
        .tool_calls
        .iter()
        .flatten()
        .zip(&b.calls)
        .find(|(_, c)| c.ids.iter().any(|i| i == id))
        .map(|(t, _)| t.tool_call_id.clone());
    b.step
        .observation
        .get_or_insert_with(|| AtifObservation {
            results: Vec::new(),
        })
        .results
        .push(AtifObservationResult {
            source_call_id: call_id,
            content: Some(content),
            is_error: false,
        });
}

fn system_step(text: String) -> AtifStep {
    AtifStep {
        step_id: 0,
        timestamp: None,
        source: "system".to_string(),
        message: AtifMessage::Text(text),
        reasoning_content: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        extra: None,
    }
}

fn step_extra(agent: String, parent_step: Option<u32>) -> AtifStepExtra {
    AtifStepExtra {
        agent,
        parent_step,
        best_of: None,
        cut_bytes: None,
    }
}

fn status_word(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Completed => "completed",
        NodeStatus::Failed => "failed",
        NodeStatus::Canceled => "was stopped",
        _ => "was still running",
    }
}

/// `lines` as one step's metrics, prompt counting cache reads and writes as
/// `final_metrics` does. `None` when nothing was spent.
fn usage_metrics(lines: &[UsageLine]) -> Option<AtifStepMetrics> {
    let sum = |f: fn(&UsageLine) -> u32| lines.iter().map(f).fold(0u32, u32::saturating_add);
    let prompt = sum(|l| l.input_tokens)
        .saturating_add(sum(|l| l.cache_read_tokens))
        .saturating_add(sum(|l| l.cache_write_tokens));
    let completion = sum(|l| l.output_tokens);
    (prompt > 0 || completion > 0).then_some(AtifStepMetrics {
        prompt_tokens: Some(prompt),
        completion_tokens: Some(completion),
        cost_usd: None,
    })
}

fn tool_step(call: &ToolCall) -> Built {
    let result = match &call.outcome {
        ToolOutcome::Running => None,
        ToolOutcome::Done { result } => Some((result.clone(), false)),
        ToolOutcome::Failed { error } => Some((error.clone(), true)),
        ToolOutcome::Cancelled => Some(("cancelled_by_user".to_string(), true)),
    };
    let step = AtifStep {
        step_id: 0,
        timestamp: None,
        source: "agent".to_string(),
        message: AtifMessage::Text(String::new()),
        reasoning_content: None,
        tool_calls: Some(vec![AtifToolCall {
            tool_call_id: call.id.clone(),
            function_name: call.name.clone(),
            arguments: call.arguments.clone().unwrap_or(serde_json::Value::Null),
            extra: plugin_extra(&call.name),
        }]),
        observation: result.map(|(content, is_error)| AtifObservation {
            results: vec![AtifObservationResult {
                source_call_id: Some(call.id.clone()),
                content: Some(content),
                is_error,
            }],
        }),
        metrics: None,
        extra: None,
    };
    let agent = call
        .arguments
        .as_ref()
        .and_then(|a| a.get("agent"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Built::new(
        step,
        vec![CallRef {
            ids: vec![call.id.clone()],
            name: call.name.clone(),
            agent,
        }],
    )
}

/// The tree a swarm export was written from: its roster, with each step's
/// tool calls given back to the agent the step names.
pub fn swarm_tree_from_atif(export: &AtifExport) -> Result<Tree<AgentNode>> {
    let roster = export
        .extra
        .as_ref()
        .and_then(|extra| extra.swarm.as_ref())
        .context("not a swarm export: no extra.swarm roster")?;
    let mut ids: Vec<NodeId> = Vec::with_capacity(roster.len());
    let mut tree: Option<Tree<AgentNode>> = None;
    for (i, agent) in roster.iter().enumerate() {
        let node = AgentNode {
            name: agent.name.clone(),
            spec: agent.spec.clone(),
            root_task_id: agent.root_task_id.clone(),
            model: agent.model.clone(),
            turns: agent.turns,
            text_bytes: agent.text_bytes,
            tool_calls: Vec::new(),
            usage: agent.usage.clone(),
            status: agent.status.clone(),
        };
        let id = match (&mut tree, agent.parent) {
            (None, None) => {
                let root = Tree::new(node);
                let id = root.root();
                tree = Some(root);
                id
            }
            (Some(tree), Some(parent)) if parent < i => tree.push(ids[parent], node),
            _ => bail!(
                "roster entry {i} ({}) is not in parents-first order",
                agent.name
            ),
        };
        ids.push(id);
    }
    let mut tree = tree.context("the swarm roster is empty")?;

    for step in &export.steps {
        let agent = step
            .extra
            .as_ref()
            .with_context(|| format!("step {} names no agent", step.step_id))?
            .agent
            .as_str();
        let Some(index) = roster.iter().position(|a| a.path == agent) else {
            bail!(
                "step {} names {agent}, who is not in the roster",
                step.step_id
            );
        };
        let results = step
            .observation
            .as_ref()
            .map_or(&[][..], |o| &o.results[..]);
        for call in step.tool_calls.iter().flatten() {
            let outcome = results
                .iter()
                .find(|r| r.source_call_id.as_deref() == Some(call.tool_call_id.as_str()))
                .map_or(ToolOutcome::Running, |r| {
                    let content = r.content.clone().unwrap_or_default();
                    if r.is_error {
                        ToolOutcome::Failed { error: content }
                    } else {
                        ToolOutcome::Done { result: content }
                    }
                });
            let plugin = call.extra.as_ref().map(|e| e.plugin.clone());
            if plugin != plugin_of(&call.function_name) {
                bail!(
                    "{}: plugin attribution does not match its name",
                    call.tool_call_id
                );
            }
            tree.get_mut(ids[index]).tool_calls.push(ToolCall {
                id: call.tool_call_id.clone(),
                name: call.function_name.clone(),
                plugin,
                arguments: (!call.arguments.is_null()).then(|| call.arguments.clone()),
                outcome,
            });
        }
    }
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionEvent;
    use chatty_fabric::{CallChain, SwarmEvent, SwarmItem};

    /// A failed call, one still running, a plugin's and one with
    /// arguments that are not JSON all read back as they were written.
    #[test]
    fn every_outcome_reads_back() {
        let mut trace = SwarmTrace::new();
        let chain = CallChain::root("t-1").extend("coder").unwrap();
        for event in [
            SessionEvent::TurnStarted,
            SessionEvent::ToolCallStarted {
                id: "a".into(),
                name: "echo__say".into(),
            },
            SessionEvent::ToolCallInput {
                id: "a".into(),
                arguments: "not json".into(),
            },
            SessionEvent::ToolCallError {
                id: "a".into(),
                error: "boom".into(),
            },
            SessionEvent::SwarmEvent(SwarmEvent {
                root_task_id: chain.root_task_id.clone(),
                node: "coder-0".into(),
                chain,
                inner: vec![
                    SwarmItem::TurnStarted,
                    SwarmItem::ToolCallStarted {
                        id: "b".into(),
                        name: "shell".into(),
                    },
                ],
            }),
        ] {
            trace.apply(&event);
        }

        let json = export_swarm(&trace).unwrap();
        assert_eq!(json["steps"][0]["tool_calls"][0]["extra"]["plugin"], "echo");
        assert_eq!(
            json["steps"][0]["observation"]["results"][0]["is_error"],
            true
        );
        assert_eq!(json["steps"][1]["extra"]["agent"], "root/coder-0");
        let parsed: AtifExport = serde_json::from_value(json).unwrap();
        assert_eq!(&swarm_tree_from_atif(&parsed).unwrap(), trace.tree());
    }
}
