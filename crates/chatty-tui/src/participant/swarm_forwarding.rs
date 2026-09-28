//! TB-1 (AGE-663) on the swarm kit: the root hears every run nested under
//! its delegation, tagged by the broker with `(root_task_id, node, chain)`
//! and batched per node, from real workers.
//!
//! The broker's side against hand-written frames — a forged tag, a burst —
//! is `chatty_protocol_gateway`'s `swarm_forwarding_tests`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use chatty_core::testing::fake_model::{Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_fabric::{FORWARD_INTERVAL, SwarmEvent, SwarmItem};
use serde_json::json;

use super::swarm_kit::{AgentDef, Endpoint, LeaderRun, SwarmKit};

const MIDDLE: &str = "kit-middle";
const MIDDLE_MODEL: &str = "kit/middle";
const GRANDCHILD: &str = "kit-grandchild";
const GRANDCHILD_MODEL: &str = "kit/grandchild";

/// The broker's batches the leader's `invoke_agent` received, in order.
fn swarm_events(run: &LeaderRun) -> Vec<SwarmEvent> {
    run.progress
        .iter()
        .filter_map(|p| match p {
            InvokeAgentProgress::Swarm(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

/// Each node's items across its batches, in order.
fn by_node(events: &[SwarmEvent]) -> BTreeMap<String, Vec<SwarmItem>> {
    let mut nodes: BTreeMap<String, Vec<SwarmItem>> = BTreeMap::new();
    for event in events {
        nodes
            .entry(event.node.clone())
            .or_default()
            .extend(event.inner.iter().cloned());
    }
    nodes
}

/// The id of `items`' `tool` start, and that its result arrived, whole.
fn tool_round_trip(items: &[SwarmItem], tool: &str, result_contains: &str) {
    let id = items
        .iter()
        .find_map(|item| match item {
            SwarmItem::ToolCallStarted { id, name } if name == tool => Some(id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {tool} start in {items:?}"));
    assert!(
        items.iter().any(|item| matches!(
            item,
            SwarmItem::ToolCallResult { id: done, result } if *done == id && result.contains(result_contains)
        )),
        "no {tool} result for {id} in {items:?}"
    );
}

/// A three-level run: the root receives the grandchild's tool events,
/// tagged with the grandchild's node and the chain the broker stamped on
/// its run. The middle worker, the root's own callee, reports to the root
/// directly and is not forwarded.
#[tokio::test]
async fn nested_events_reach_the_root_tagged() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(MIDDLE, MIDDLE_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(GRANDCHILD, GRANDCHILD_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            MIDDLE_MODEL,
            [
                Reply::tool_call(
                    "invoke_agent",
                    json!({ "agent": GRANDCHILD, "prompt": "read the readme" }),
                ),
                Reply::text("The grandchild read it."),
            ],
        ),
        Script::new().route(
            GRANDCHILD_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                Reply::text("It says Chatty."),
            ],
        ),
    )
    .await;
    let run = kit.run_leader_to(MIDDLE, "ask the grandchild").await;
    let out = run
        .output
        .as_ref()
        .expect("the nested delegation succeeded");
    assert!(out.success);
    assert_eq!(out.response, "The grandchild read it.");

    let events = swarm_events(&run);
    assert!(
        !events.is_empty(),
        "the grandchild's events reached the root"
    );
    let root_task_id = &events[0].root_task_id;
    for event in &events {
        assert_eq!(event.node, "kit-grandchild-0", "only the nested run");
        assert_eq!(&event.root_task_id, root_task_id);
        assert_eq!(event.chain.root_task_id, *root_task_id);
        assert_eq!(event.chain.chain, ["root", MIDDLE, GRANDCHILD]);
        assert_eq!(event.chain.depth, 2);
    }

    let nodes = by_node(&events);
    let items = &nodes["kit-grandchild-0"];
    assert_eq!(items.first(), Some(&SwarmItem::TurnStarted));
    tool_round_trip(items, "read_file", "# Chatty");
    assert!(
        items.iter().any(|item| matches!(
            item,
            SwarmItem::Text { bytes } if *bytes == "It says Chatty.".len() as u64
        )),
        "the answer, as its length: {items:?}"
    );
    let usage = items
        .iter()
        .find_map(|item| match item {
            SwarmItem::Usage { usage } => Some(usage.clone()),
            _ => None,
        })
        .expect("the grandchild's usage, whole");
    assert!(usage["inputTokens"].as_u64().unwrap_or(0) > 0, "{usage}");
    assert_eq!(
        items.last(),
        Some(&SwarmItem::Ended {
            state: "completed".into()
        })
    );

    // The delegation's own progress is what it was: the grandchild's steps
    // inside the middle worker's, then the answer.
    assert!(run.progress.iter().any(|p| matches!(
        p,
        InvokeAgentProgress::Step(step) if step == "read_file"
    )));
}

const LEAVES: usize = 19;

/// A 20-node run — a sub-leader and nineteen leaves under it — forwards at
/// most `4 × nodes × duration_s` batches to the root, and no text chunk
/// crosses a hop: the leaves' answers reach the root only as byte counts.
/// Counted against the run's own length; nothing here asserts a time.
#[tokio::test]
async fn forwarding_is_bounded() {
    let leaves: Vec<(String, String)> = (0..LEAVES)
        .map(|i| (format!("kit-leaf-{i:02}"), format!("kit/leaf-{i:02}")))
        .collect();
    let mut roster = vec![AgentDef::new(MIDDLE, MIDDLE_MODEL, Endpoint::Sse).sub_leader()];
    let mut ndjson = Script::new();
    for (name, model) in &leaves {
        roster.push(AgentDef::new(name, model, Endpoint::Ndjson));
        ndjson = ndjson.route(
            model.clone(),
            [
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                Reply::text(format!("secret answer of {name}")),
            ],
        );
    }
    let calls = leaves
        .iter()
        .map(|(name, _)| {
            (
                "invoke_agent".to_string(),
                json!({ "agent": name, "prompt": "read the readme" }),
            )
        })
        .collect();
    let sse = Script::new().route(
        MIDDLE_MODEL,
        [Reply::ToolCalls(calls), Reply::text("All read.")],
    );
    let kit = SwarmKit::start(roster, sse, ndjson).await;

    let started = Instant::now();
    let run = kit.run_leader_to(MIDDLE, "have every leaf read").await;
    let duration_s = started.elapsed().as_secs_f64();
    let out = run.output.as_ref().expect("the run succeeded");
    assert!(out.success, "{out:?}");

    let events = swarm_events(&run);
    let nodes = 1 + LEAVES;
    let bound = 4.0 * nodes as f64 * duration_s;
    assert!(
        (events.len() as f64) <= bound,
        "{} forwarded events for {nodes} nodes over {duration_s:.2}s (bound {bound:.0})",
        events.len()
    );
    // Per node, too: its batches are an interval apart.
    let per_node_bound = duration_s / FORWARD_INTERVAL.as_secs_f64();
    let mut batches: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &events {
        *batches.entry(event.node.as_str()).or_default() += 1;
    }
    for (node, count) in &batches {
        assert!(
            *count as f64 <= per_node_bound,
            "{node}: {count} batches in {per_node_bound:.1} intervals"
        );
    }

    // Every leaf reached the root, its tool events whole.
    let by_node = by_node(&events);
    let reached: BTreeSet<&str> = by_node.keys().map(String::as_str).collect();
    let expected: BTreeSet<String> = leaves.iter().map(|(name, _)| format!("{name}-0")).collect();
    assert_eq!(
        reached,
        expected.iter().map(String::as_str).collect::<BTreeSet<_>>()
    );
    for (node, items) in &by_node {
        tool_round_trip(items, "read_file", "# Chatty");
        assert_eq!(
            items.last(),
            Some(&SwarmItem::Ended {
                state: "completed".into()
            }),
            "{node}"
        );
    }

    // No text chunk crossed a hop: nothing forwarded carries an answer.
    let forwarded = serde_json::to_string(&events).unwrap();
    assert!(!forwarded.contains("secret answer"), "{forwarded}");
}
