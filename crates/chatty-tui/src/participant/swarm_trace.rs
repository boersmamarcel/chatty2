//! TB-2 (AGE-664) on the swarm kit: a leader → reviewer → coder run, with
//! real workers on the fake model, folded into one `SwarmTrace` and
//! exported as one ATIF document.
//!
//! The kit's leader is the in-process root's `invoke_agent` call, not a
//! model turn, so the root's own events around it — its plugin call, its
//! delegation, its usage — are scripted here, the way `AgentSession`'s
//! handler emits them. Everything under the root is real: the broker's
//! tagged batches, the workers' usage as the fake server served it, and
//! the broker's edge log.

use chatty_core::exporters::types::AtifExport;
use chatty_core::exporters::{export_swarm, swarm_tree_from_atif};
use chatty_core::models::token_usage::{ModelRef, TokenUsage};
use chatty_core::services::swarm_trace::{
    AgentNode, NodeStatus, SwarmTrace, ToolOutcome, UsageLine,
};
use chatty_core::session::SessionEvent;
use chatty_core::settings::models::providers_store::ProviderType;
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_fabric::EdgeRow;
use serde_json::json;

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

const REVIEWER: &str = "kit-reviewer";
const REVIEWER_MODEL: &str = "kit/reviewer";
const CODER: &str = "kit-coder";
const CODER_MODEL: &str = "kit/coder";
const LEADER_MODEL: &str = "kit/leader";

/// `(input, output, cache_read)` of each answer the fake server gives the
/// reviewer, in order: it reads the readme, asks the coder, answers.
const REVIEWER_USAGE: [(u64, u64, u64); 3] = [(100, 20, 7), (150, 10, 0), (200, 30, 0)];
/// The coder's: it reads the readme, answers. Ollama reports no cache.
const CODER_USAGE: [(u64, u64, u64); 2] = [(40, 8, 0), (60, 12, 0)];

/// The leader's own turn: one plugin call and its request.
const PLUGIN_TOOL: &str = "benford__analyze";
const LEADER_LINE: (u32, u32) = (50, 10);
const PLUGIN_LINE: (u32, u32) = (5, 1);

fn usage((input, output, cache_read): (u64, u64, u64)) -> Reply {
    Reply::Usage {
        input,
        output,
        cache_read,
    }
}

/// What the fake server served one agent, summed:
/// `(input, output, cache_read)`.
fn served(answers: &[(u64, u64, u64)]) -> (u32, u32, u32) {
    answers.iter().fold((0, 0, 0), |(i, o, c), (a, b, d)| {
        (i + *a as u32, o + *b as u32, c + *d as u32)
    })
}

/// A node's own spend, summed over its lines: `(input, output, cache_read)`.
fn spent(node: &AgentNode) -> (u32, u32, u32) {
    node.usage.iter().fold((0, 0, 0), |(i, o, c), l| {
        (
            i + l.input_tokens,
            o + l.output_tokens,
            c + l.cache_read_tokens,
        )
    })
}

fn sum(lines: &[UsageLine]) -> (u32, u32, u32, u32) {
    lines.iter().fold((0, 0, 0, 0), |(i, o, r, w), l| {
        (
            i + l.input_tokens,
            o + l.output_tokens,
            r + l.cache_read_tokens,
            w + l.cache_write_tokens,
        )
    })
}

/// A run, as the root session and the broker saw it.
///
/// `pub(crate)` so TB-5's `/swarm` golden (`tui_swarm.rs`) can script the
/// same three-level run without duplicating the kit setup.
pub(crate) struct Run {
    pub(crate) events: Vec<SessionEvent>,
    pub(crate) edges: Vec<EdgeRow>,
    /// The fake server's request count per agent's model.
    requests: [(usize, usize); 2],
}

/// The leader delegates to the reviewer, which reads the readme and asks
/// the coder, which reads it too.
pub(crate) async fn leader_reviewer_coder() -> Run {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(REVIEWER, REVIEWER_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(CODER, CODER_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            REVIEWER_MODEL,
            [
                usage(REVIEWER_USAGE[0]),
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                usage(REVIEWER_USAGE[1]),
                Reply::tool_call(
                    "invoke_agent",
                    json!({ "agent": CODER, "prompt": "read the readme" }),
                ),
                usage(REVIEWER_USAGE[2]),
                Reply::text("Reviewed: it says Chatty."),
            ],
        ),
        Script::new().route(
            CODER_MODEL,
            [
                usage(CODER_USAGE[0]),
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                usage(CODER_USAGE[1]),
                Reply::text("It says Chatty."),
            ],
        ),
    )
    .await;
    let run = kit.run_leader_to(REVIEWER, "review the readme").await;
    let out = run.output.as_ref().expect("the delegation succeeded");
    assert!(out.success, "{out:?}");

    let leader = ModelRef {
        provider: ProviderType::Ollama,
        model_id: LEADER_MODEL.to_string(),
    };
    let mut events = vec![
        SessionEvent::TurnStarted,
        SessionEvent::ToolCallStarted {
            id: "leader-1".into(),
            name: PLUGIN_TOOL.into(),
        },
        SessionEvent::ToolCallInput {
            id: "leader-1".into(),
            arguments: r#"{"column":"amount"}"#.into(),
        },
        SessionEvent::ToolCallResult {
            id: "leader-1".into(),
            result: "conforms".into(),
        },
        SessionEvent::ToolCallStarted {
            id: "leader-2".into(),
            name: "invoke_agent".into(),
        },
        SessionEvent::ToolCallInput {
            id: "leader-2".into(),
            arguments: json!({ "agent": REVIEWER, "prompt": "review the readme" }).to_string(),
        },
    ];
    // As the session's handler emits the tool's progress.
    events.extend(run.progress.iter().cloned().map(|p| match p {
        InvokeAgentProgress::Swarm(batch) => SessionEvent::SwarmEvent(batch),
        p => SessionEvent::Delegation(p),
    }));
    events.extend([
        SessionEvent::ToolCallResult {
            id: "leader-2".into(),
            result: out.response.clone(),
        },
        SessionEvent::Text(out.response.clone()),
        SessionEvent::PluginUsage(TokenUsage {
            plugin: Some("benford".into()),
            model: Some(leader.clone()),
            ..TokenUsage::new(PLUGIN_LINE.0, PLUGIN_LINE.1)
        }),
        SessionEvent::TokenUsage(TokenUsage {
            model: Some(leader),
            ..TokenUsage::new(LEADER_LINE.0, LEADER_LINE.1)
        }),
        SessionEvent::TurnEnded,
    ]);

    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    let edges = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("an edge row"))
        .collect();
    let requests = [
        (
            kit.sse.requests_for(REVIEWER_MODEL).len(),
            REVIEWER_USAGE.len(),
        ),
        (
            kit.ndjson.requests_for(CODER_MODEL).len(),
            CODER_USAGE.len(),
        ),
    ];
    Run {
        events,
        edges,
        requests,
    }
}

/// Invariant 1: three nodes, each billed what the fake server served that
/// agent — the reviewer, a sub-leader whose report has the coder's usage
/// folded in, is billed only its own — and the nodes sum to the root's
/// total. The tree built live, event by event, is the finished one.
#[tokio::test]
async fn swarm_tree_spend_sums() {
    let run = leader_reviewer_coder().await;
    for (got, scripted) in run.requests {
        assert_eq!(got, scripted, "every scripted answer was served");
    }
    let trace = SwarmTrace::from_edges(&run.edges, &run.events);
    let tree = trace.tree();

    assert_eq!(tree.len(), 3, "{tree:#?}");
    let root = tree.root();
    let reviewer = trace.node_named("kit-reviewer-0").expect("the reviewer");
    let coder = trace.node_named("kit-coder-0").expect("the coder");
    assert_eq!(tree.children(root), [reviewer]);
    assert_eq!(tree.children(reviewer), [coder]);
    assert_eq!(tree.get(reviewer).spec, REVIEWER);
    assert_eq!(tree.get(coder).spec, CODER);
    for id in [root, reviewer, coder] {
        assert_eq!(tree.get(id).status, NodeStatus::Completed, "{id:?}");
    }
    assert_eq!(
        tree.get(reviewer).model.as_ref().unwrap().model_id,
        REVIEWER_MODEL
    );
    assert_eq!(
        tree.get(coder).model.as_ref().unwrap().model_id,
        CODER_MODEL
    );

    assert_eq!(spent(tree.get(reviewer)), served(&REVIEWER_USAGE));
    assert_eq!(spent(tree.get(coder)), served(&CODER_USAGE));
    let (i, o) = (LEADER_LINE.0 + PLUGIN_LINE.0, LEADER_LINE.1 + PLUGIN_LINE.1);
    assert_eq!(spent(tree.get(root)), (i, o, 0));
    assert!(
        tree.get(root)
            .usage
            .iter()
            .any(|l| l.plugin.as_deref() == Some("benford")),
        "the plugin's line is the root's, under its name"
    );

    // The nodes sum to the root's total: its own lines plus what its
    // delegation reported, as the conversation records them.
    assert_eq!(sum(&trace.total()), sum(&trace.billed()));
    let (ri, ro, rc) = served(&REVIEWER_USAGE);
    let (ci, co, cc) = served(&CODER_USAGE);
    assert_eq!(sum(&trace.total()), (i + ri + ci, o + ro + co, rc + cc, 0));

    // Built live, one event at a time, it is the same tree: the root's
    // callee is named by its spec until the edge log names its node.
    let mut live = SwarmTrace::new();
    let mut revisions = Vec::new();
    for event in &run.events {
        live.apply(event);
        revisions.push(live.revision());
    }
    assert!(revisions.windows(2).all(|w| w[0] <= w[1]));
    let callee = live.node_named(REVIEWER).expect("named by its spec");
    assert_eq!(spent(live.tree().get(callee)), served(&REVIEWER_USAGE));
    for row in &run.edges {
        live.apply_edge(row);
    }
    assert_eq!(live.tree(), trace.tree());
}

/// Invariant 2: the one ATIF document holds every tool call of all three
/// agents, each step naming its agent and the plugin call its plugin, and
/// it parses back into the exporter's own types and into the same tree.
#[tokio::test]
async fn swarm_atif_round_trip() {
    let run = leader_reviewer_coder().await;
    let trace = SwarmTrace::from_edges(&run.edges, &run.events);
    let json = export_swarm(&trace).expect("the export").to_string();

    let parsed: AtifExport = serde_json::from_str(&json).expect("the export parses back");
    let calls: Vec<(String, String, Option<String>)> = parsed
        .steps
        .iter()
        .flat_map(|step| {
            let agent = step
                .extra
                .as_ref()
                .expect("every step names its agent")
                .agent
                .clone();
            step.tool_calls.iter().flatten().map(move |call| {
                (
                    agent.clone(),
                    call.function_name.clone(),
                    call.extra.as_ref().map(|e| e.plugin.clone()),
                )
            })
        })
        .collect();
    let expected: Vec<(String, String, Option<String>)> = [
        ("root", PLUGIN_TOOL, Some("benford")),
        ("root", "invoke_agent", None),
        ("kit-reviewer-0", "read_file", None),
        ("kit-reviewer-0", "invoke_agent", None),
        ("kit-coder-0", "read_file", None),
    ]
    .into_iter()
    .map(|(a, t, p)| (a.to_string(), t.to_string(), p.map(str::to_string)))
    .collect();
    assert_eq!(calls, expected);

    // Every tool call in the tree is in the export, finished.
    let tree = trace.tree();
    let in_tree: usize = tree
        .preorder()
        .into_iter()
        .map(|id| tree.get(id).tool_calls.len())
        .sum();
    assert_eq!(in_tree, expected.len());
    for id in tree.preorder() {
        for call in &tree.get(id).tool_calls {
            assert!(matches!(call.outcome, ToolOutcome::Done { .. }), "{call:?}");
        }
    }

    let back = swarm_tree_from_atif(&parsed).expect("a swarm export");
    assert_eq!(&back, tree);
}
