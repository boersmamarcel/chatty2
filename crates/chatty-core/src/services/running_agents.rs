//! Every live agent across conversations (TB-6, AGE-748).
//!
//! Each conversation's turn folds its events into a [`SwarmTrace`] (TB-2):
//! the broker's tagged batches, stamped from its task table, and the
//! root's own delegations. [`RunningAgents`] keeps the latest trace of every
//! conversation whose turn is still streaming and lists the nodes in them
//! that are still running, as [`RunningAgent`] rows: agent, conversation,
//! parent chain, whether it runs or waits on the human, since when, and
//! what its subtree has reported spending.
//!
//! Nothing here polls. A frontend hands in a conversation's trace whenever
//! that trace changed (the desktop's `SwarmTreeChanged`, the TUI's own
//! fold) and drops the conversation when its turn ends; the rows are
//! computed from what was handed in. Elapsed time is read against a `now`
//! the caller passes, so a view can redraw its clock without new data.
//!
//! The root is not a row: it is the conversation itself, and the
//! overview is of the agents it started.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::models::token_usage::{PriceBook, format_cost, format_tokens, price};
use crate::services::swarm_trace::{NodeId, SwarmTrace, UsageLine};
use crate::tools::invoke_agent_tool::WaitingOn;

/// Where a live agent is: working, or waiting on the human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Running,
    Waiting(WaitingOn),
}

impl Activity {
    /// The status column's words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting(WaitingOn::Approval) => "waiting on approval",
            Self::Waiting(WaitingOn::Answer) => "waiting on your answer",
        }
    }

    pub fn is_waiting(self) -> bool {
        matches!(self, Self::Waiting(_))
    }
}

/// One live agent: one row of the overview.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningAgent {
    /// The conversation whose turn started it.
    pub conversation_id: String,
    /// That conversation's title, as the frontend named it.
    pub conversation: String,
    /// The name the broker admitted its run under (`<spec>-<n>`): what
    /// Stop and jump-to address.
    pub agent: String,
    pub spec: String,
    /// The agents above it, the root first and its direct caller last.
    pub chain: Vec<String>,
    pub activity: Activity,
    /// When its run was first seen; `None` only for a trace that never
    /// stamped it.
    pub started: Option<SystemTime>,
    /// What it and everything under it have reported spending so far.
    pub spend: Vec<UsageLine>,
}

impl RunningAgent {
    /// How long it has run, as of `now`.
    pub fn elapsed(&self, now: SystemTime) -> Duration {
        self.started
            .and_then(|started| now.duration_since(started).ok())
            .unwrap_or_default()
    }

    /// The parent chain as one line: `root › lead-0`.
    pub fn chain_text(&self) -> String {
        self.chain.join(" \u{203a} ")
    }
}

/// The running nodes of one conversation's `trace`, preorder; none once
/// its root's turn is over, which ends every run under it.
pub fn rows_of(conversation_id: &str, conversation: &str, trace: &SwarmTrace) -> Vec<RunningAgent> {
    let tree = trace.tree();
    if !tree.get(tree.root()).status.is_running() {
        return Vec::new();
    }
    tree.preorder()
        .into_iter()
        .filter(|&id| id != tree.root() && tree.get(id).status.is_running())
        .map(|id| {
            let node = tree.get(id);
            RunningAgent {
                conversation_id: conversation_id.to_string(),
                conversation: conversation.to_string(),
                agent: node.name.clone(),
                spec: node.spec.clone(),
                chain: ancestors(trace, id),
                activity: trace
                    .waiting_on(id)
                    .map_or(Activity::Running, Activity::Waiting),
                started: trace.first_seen(id),
                spend: trace.subtree_spend(id),
            }
        })
        .collect()
}

/// The names above `id`, the root first.
fn ancestors(trace: &SwarmTrace, id: NodeId) -> Vec<String> {
    let tree = trace.tree();
    let mut chain = Vec::new();
    let mut at = tree.parent(id);
    while let Some(parent) = at {
        chain.push(tree.get(parent).name.clone());
        at = tree.parent(parent);
    }
    chain.reverse();
    chain
}

/// One conversation whose turn is still streaming.
#[derive(Debug, Clone)]
struct Live {
    title: String,
    trace: Arc<SwarmTrace>,
}

/// The latest swarm of every conversation whose turn is streaming. See
/// the module docs.
#[derive(Debug, Clone, Default)]
pub struct RunningAgents {
    live: BTreeMap<String, Live>,
}

impl RunningAgents {
    pub fn new() -> Self {
        Self::default()
    }

    /// `conversation_id`'s swarm is now `trace`.
    pub fn update(&mut self, conversation_id: &str, title: &str, trace: Arc<SwarmTrace>) {
        self.live.insert(
            conversation_id.to_string(),
            Live {
                title: title.to_string(),
                trace,
            },
        );
    }

    /// `conversation_id`'s turn ended: nothing in it runs any more.
    /// Whether it had any swarm here.
    pub fn end(&mut self, conversation_id: &str) -> bool {
        self.live.remove(conversation_id).is_some()
    }

    /// `conversation_id`'s latest swarm, while its turn streams.
    pub fn trace(&self, conversation_id: &str) -> Option<&Arc<SwarmTrace>> {
        self.live.get(conversation_id).map(|live| &live.trace)
    }

    /// Every live agent, conversation by conversation, each in tree order.
    pub fn rows(&self) -> Vec<RunningAgent> {
        self.live
            .iter()
            .flat_map(|(id, live)| rows_of(id, &live.title, &live.trace))
            .collect()
    }

    /// How many agents are live, and how many of them wait on the human.
    pub fn counts(&self) -> (usize, usize) {
        let rows = self.rows();
        let waiting = rows.iter().filter(|row| row.activity.is_waiting()).count();
        (rows.len(), waiting)
    }
}

/// `elapsed` as a clock reads it: `45s`, `3m 07s`, `1h 02m`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// A row's spend: its tokens, and their price when `book` prices every
/// line; `—` before anything under it has reported.
pub fn spend_text(spend: &[UsageLine], book: Option<&PriceBook>) -> String {
    let tokens: u64 = spend.iter().map(UsageLine::tokens).sum();
    if tokens == 0 {
        return "\u{2014}".to_string();
    }
    let tokens = format_tokens(u32::try_from(tokens).unwrap_or(u32::MAX));
    let cost = book.map(|book| {
        let lines: Vec<_> = spend.iter().map(UsageLine::to_token_usage).collect();
        price(&lines, book)
    });
    match cost {
        Some(cost) if cost.unpriced_lines == 0 => {
            format!("{tokens} tok \u{b7} {}", format_cost(cost.usd))
        }
        _ => format!("{tokens} tok"),
    }
}

/// When nothing runs.
pub const NONE_RUNNING: &str = "No agents are running.";

/// The rows as text, one agent per line — `/agents running`'s output:
/// `<agent> · <conversation> · <chain> · <status> · <elapsed> · <spend>`.
pub fn render_text(rows: &[RunningAgent], now: SystemTime, book: Option<&PriceBook>) -> String {
    if rows.is_empty() {
        return NONE_RUNNING.to_string();
    }
    rows.iter()
        .map(|row| {
            format!(
                "{} \u{b7} {} \u{b7} {} \u{b7} {} \u{b7} {} \u{b7} {}",
                row.agent,
                row.conversation,
                row.chain_text(),
                row.activity.label(),
                format_elapsed(row.elapsed(now)),
                spend_text(&row.spend, book),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::message_types::ToolSource;
    use crate::session::SessionEvent;
    use crate::tools::invoke_agent_tool::InvokeAgentProgress;
    use chatty_fabric::{CallChain, SwarmEvent, SwarmItem};

    fn started(agent: &str) -> SessionEvent {
        SessionEvent::Delegation(InvokeAgentProgress::Started {
            agent_name: agent.to_string(),
            prompt: "go".to_string(),
            source: ToolSource::Local,
        })
    }

    fn admitted(node: &str) -> SessionEvent {
        SessionEvent::Delegation(InvokeAgentProgress::Admitted(node.to_string()))
    }

    /// A batch from `node`, nested under the root's callee `chain[1]`.
    fn batch(node: &str, chain: &[&str], item: SwarmItem) -> SessionEvent {
        let mut call_chain = CallChain::root("task-1");
        for spec in &chain[1..] {
            call_chain = call_chain.extend(spec).expect("a short chain");
        }
        SessionEvent::SwarmEvent(SwarmEvent {
            root_task_id: "task-1".to_string(),
            node: node.to_string(),
            chain: call_chain,
            inner: vec![item],
        })
    }

    fn trace(events: &[SessionEvent]) -> Arc<SwarmTrace> {
        let mut trace = SwarmTrace::new();
        for event in events {
            trace.apply(event);
        }
        Arc::new(trace)
    }

    fn names(rows: &[RunningAgent]) -> Vec<(&str, &str, String, Activity)> {
        rows.iter()
            .map(|row| {
                (
                    row.conversation_id.as_str(),
                    row.agent.as_str(),
                    row.chain_text(),
                    row.activity,
                )
            })
            .collect()
    }

    /// TB-6: two conversations stream at once, each with its own swarm;
    /// the overview lists the live nodes of both — with their conversation
    /// and parent chain — and drops a finished node and an ended turn.
    #[test]
    fn running_agents_lists_live_nodes_across_conversations() {
        let mut running = RunningAgents::new();
        running.update(
            "conv-a",
            "Fix the build",
            trace(&[
                SessionEvent::TurnStarted,
                started("lead"),
                admitted("lead-0"),
                batch(
                    "coder-0",
                    &["root", "lead", "coder"],
                    SwarmItem::TurnStarted,
                ),
                batch(
                    "tester-0",
                    &["root", "lead", "tester"],
                    SwarmItem::TurnStarted,
                ),
                batch(
                    "tester-0",
                    &["root", "lead", "tester"],
                    SwarmItem::Ended {
                        state: "completed".to_string(),
                    },
                ),
            ]),
        );
        running.update(
            "conv-b",
            "Write the docs",
            trace(&[
                SessionEvent::TurnStarted,
                started("writer"),
                admitted("writer-0"),
            ]),
        );

        let rows = running.rows();
        assert_eq!(
            names(&rows),
            [
                ("conv-a", "lead-0", "root".to_string(), Activity::Running),
                (
                    "conv-a",
                    "coder-0",
                    "root \u{203a} lead-0".to_string(),
                    Activity::Running
                ),
                ("conv-b", "writer-0", "root".to_string(), Activity::Running),
            ],
            "the finished tester is not live"
        );
        assert_eq!(rows[0].conversation, "Fix the build");
        assert_eq!(rows[2].conversation, "Write the docs");
        assert!(rows.iter().all(|row| row.started.is_some()));
        assert_eq!(running.counts(), (3, 0));

        assert!(running.end("conv-a"));
        assert_eq!(
            names(&running.rows()),
            [("conv-b", "writer-0", "root".to_string(), Activity::Running)]
        );
        let text = render_text(&running.rows(), SystemTime::now(), None);
        assert!(
            text.starts_with("writer-0 \u{b7} Write the docs \u{b7} root \u{b7} running \u{b7} "),
            "{text}"
        );
        running.end("conv-b");
        assert_eq!(
            render_text(&running.rows(), SystemTime::now(), None),
            NONE_RUNNING
        );
    }

    #[test]
    fn a_wait_lasts_until_resumed_and_names_what_it_waits_for() {
        let mut events = vec![
            SessionEvent::TurnStarted,
            started("lead"),
            admitted("lead-0"),
            batch(
                "coder-0",
                &["root", "lead", "coder"],
                SwarmItem::TurnStarted,
            ),
            SessionEvent::Delegation(InvokeAgentProgress::Waiting {
                id: "question-1".to_string(),
                agent: "coder-0".to_string(),
                on: WaitingOn::Answer,
            }),
        ];
        let rows = rows_of("c", "t", &trace(&events));
        assert_eq!(rows[0].activity, Activity::Running);
        assert_eq!(rows[1].activity, Activity::Waiting(WaitingOn::Answer));
        assert_eq!(rows[1].activity.label(), "waiting on your answer");

        events.push(SessionEvent::Delegation(InvokeAgentProgress::Resumed {
            id: "question-1".to_string(),
        }));
        let rows = rows_of("c", "t", &trace(&events));
        assert_eq!(rows[1].activity, Activity::Running);
    }

    #[test]
    fn elapsed_reads_like_a_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(45)), "45s");
        assert_eq!(format_elapsed(Duration::from_secs(187)), "3m 07s");
        assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h 02m");
        assert_eq!(spend_text(&[], None), "\u{2014}");
    }
}
