//! One ATIF document for a whole swarm (TB-2, AGE-664).
//!
//! [`export_swarm`] writes a [`SwarmTrace`] as one trajectory: a step per
//! tool call, each naming the agent that made it in `steps[].extra.agent`,
//! and a plugin tool's call naming its plugin in `tool_calls[].extra.plugin`
//! — AGE-5's per-module attribution, per agent and plugin. The agents
//! themselves, with their place in the tree and their own spend, are the
//! roster in `extra.swarm`. [`swarm_tree_from_atif`] reads a document back
//! into the tree it was written from.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::SCHEMA_VERSION;
use super::steps::plugin_extra;
use crate::exporters::types::*;
use crate::services::swarm_trace::{
    AgentNode, NodeId, SwarmTrace, ToolCall, ToolOutcome, Tree, UsageLine, plugin_of,
};

/// `trace` as one ATIF document. Usage is written as facts (tokens per
/// model) and never priced here: cost is computed on read (AGE-682).
pub fn export_swarm(trace: &SwarmTrace) -> Result<serde_json::Value> {
    serde_json::to_value(swarm_to_atif(trace))
        .context("Failed to serialize the swarm's ATIF export")
}

/// [`export_swarm`], typed.
pub fn swarm_to_atif(trace: &SwarmTrace) -> AtifExport {
    let tree = trace.tree();
    let order = tree.preorder();
    let index: BTreeMap<NodeId, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    let mut steps = Vec::new();
    for id in &order {
        let node = tree.get(*id);
        for call in &node.tool_calls {
            steps.push(tool_step(steps.len() as u32 + 1, &node.name, call));
        }
    }

    let roster = order
        .iter()
        .map(|id| {
            let node = tree.get(*id);
            AtifSwarmAgent {
                name: node.name.clone(),
                spec: node.spec.clone(),
                parent: tree.parent(*id).map(|p| index[&p]),
                root_task_id: node.root_task_id.clone(),
                model: node.model.clone(),
                turns: node.turns,
                text_bytes: node.text_bytes,
                usage: node.usage.clone(),
                status: node.status.clone(),
            }
        })
        .collect();

    let total = trace.total();
    let sum = |f: fn(&UsageLine) -> u32| total.iter().map(f).fold(0u32, u32::saturating_add);
    let (cache_read, cache_write) = (sum(|l| l.cache_read_tokens), sum(|l| l.cache_write_tokens));
    let root = tree.get(tree.root());
    let session_id = match trace.root_task_ids().as_slice() {
        [] => root.name.clone(),
        ids => ids.join(","),
    };

    AtifExport {
        schema_version: SCHEMA_VERSION.to_string(),
        session_id,
        agent: AtifAgent {
            name: "chatty".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            model_name: root.model.as_ref().map(|m| m.model_id.clone()),
            extra: None,
        },
        final_metrics: Some(AtifFinalMetrics {
            total_prompt_tokens: Some(
                sum(|l| l.input_tokens)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write),
            ),
            total_completion_tokens: Some(sum(|l| l.output_tokens)),
            total_cost_usd: None,
            total_steps: Some(steps.len() as u32),
            extra: AtifFinalMetricsExtra::from_totals(cache_read, cache_write),
        }),
        steps,
        extra: Some(AtifExtra {
            feedback: Vec::new(),
            regenerations: Vec::new(),
            swarm: Some(roster),
        }),
    }
}

fn tool_step(step_id: u32, agent: &str, call: &ToolCall) -> AtifStep {
    let result = match &call.outcome {
        ToolOutcome::Running => None,
        ToolOutcome::Done { result } => Some((result.clone(), false)),
        ToolOutcome::Failed { error } => Some((error.clone(), true)),
    };
    AtifStep {
        step_id,
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
        extra: Some(AtifStepExtra {
            agent: agent.to_string(),
        }),
    }
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
        let Some(index) = roster.iter().position(|a| a.name == agent) else {
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
        assert_eq!(json["steps"][1]["extra"]["agent"], "coder-0");
        let parsed: AtifExport = serde_json::from_value(json).unwrap();
        assert_eq!(&swarm_tree_from_atif(&parsed).unwrap(), trace.tree());
    }
}
