//! The broker-stamped call chain (PL-S2 DP-2, AGE-643) on the swarm kit:
//! real worker processes, a scripted fake model behind them, and a broker
//! that refuses a cycle, a call deeper than four and a call the specs do
//! not allow before anything is spawned.
//!
//! "Nothing is spawned" is read three ways: the broker admitted no node for
//! the refused callee (a runner admits one per worker process it starts),
//! the callee's model got no request, and the only edge-log row the call
//! wrote is its refusal. The refusal reaches the calling worker's model as
//! its tool result, in the exact text `refusal_text_golden` pins.
//!
//! `forged_chain_is_ignored` needs a worker that writes its own call frame,
//! which a real one never does; it lives with the broker's call path in
//! `chatty-protocol-gateway` (`participant/calls.rs`).

use chatty_core::agent_spec::AgentSpec;
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};

use super::swarm_kit::{AgentDef, Endpoint, SwarmKit};

/// The broker's edge log as `(kind, from, to, outcome)`, in the order the
/// calls ended.
fn edges(kit: &SwarmKit) -> Vec<(String, String, String, String)> {
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    log.lines()
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).expect("a JSON row");
            let field = |name: &str| row[name].as_str().unwrap_or_default().to_string();
            (field("kind"), field("from"), field("to"), field("outcome"))
        })
        .collect()
}

fn row(kind: &str, from: &str, to: &str, outcome: &str) -> (String, String, String, String) {
    (kind.into(), from.into(), to.into(), outcome.into())
}

fn invoke(agent: &str, prompt: &str) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        serde_json::json!({ "agent": agent, "prompt": prompt }),
    )
}

/// Whether `request` carries `text` — the tool result the model reads.
fn carries(request: &RecordedRequest, text: &str) -> bool {
    request.json().to_string().contains(text)
}

/// Invariant 2: root → a → b → a. The second `a` is refused with `cycle`
/// before it is spawned: no node, no model request, one refusal row. `b`'s
/// model reads the refusal and answers; the run completes.
#[tokio::test]
async fn cycle_refused_before_spawn() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new("kit-a", "kit/a", Endpoint::Sse).sub_leader(),
            AgentDef::new("kit-b", "kit/b", Endpoint::Ndjson).sub_leader(),
        ],
        Script::new().route(
            "kit/a",
            [invoke("kit-b", "Check it."), Reply::text("a is done.")],
        ),
        Script::new().route(
            "kit/b",
            [
                invoke("kit-a", "Loop back."),
                Reply::text("b could not loop back."),
            ],
        ),
    )
    .await;

    let run = kit.run_leader("Start.").await;
    let out = run.output.as_ref().expect("the run completes");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "a is done.");

    assert_eq!(
        kit.participants().admitted(),
        ["kit-a-0", "kit-b-0"],
        "no second kit-a was admitted, so none was spawned"
    );
    assert_eq!(
        kit.sse.requests_for("kit/a").len(),
        2,
        "only the first kit-a's two turns reached the model"
    );
    let b = kit.ndjson.requests_for("kit/b");
    assert_eq!(b.len(), 2);
    assert!(
        carries(
            &b[1],
            "Error: invoke_agent: cycle: root \u{2192} kit-a \u{2192} kit-b \u{2192} kit-a"
        ),
        "kit-b's model reads the refusal: {}",
        b[1].json()
    );

    assert_eq!(
        edges(&kit),
        [
            row(
                "refusal",
                "kit-b-0",
                "kit-a",
                "cycle: root \u{2192} kit-a \u{2192} kit-b \u{2192} kit-a"
            ),
            row("task", "kit-a-0", "kit-b-0", "completed"),
            row("task", "root", "kit-a-0", "completed"),
        ]
    );
    assert_eq!(kit.participants().open_runs(), 0, "every run released");
}

/// Invariant 3: root → 1 → 2 → 3 → 4 runs; the fifth level is refused at
/// depth 5 before it is spawned.
#[tokio::test]
async fn depth_limit_enforced() {
    // Alternate endpoints at the default budget of one: a level waiting on
    // its call does not hold its slot (BI-6).
    let roster = (1..=5)
        .map(|n| {
            let endpoint = if n % 2 == 1 {
                Endpoint::Sse
            } else {
                Endpoint::Ndjson
            };
            AgentDef::new(&format!("kit-{n}"), &format!("kit/{n}"), endpoint).sub_leader()
        })
        .collect();
    let step = |n: u8| {
        [
            invoke(&format!("kit-{}", n + 1), "Go one deeper."),
            Reply::text(format!("level {n} done.")),
        ]
    };
    let kit = SwarmKit::start(
        roster,
        Script::new()
            .route("kit/1", step(1))
            .route("kit/3", step(3)),
        Script::new()
            .route("kit/2", step(2))
            .route("kit/4", step(4)),
    )
    .await;

    let run = kit.run_leader("Start.").await;
    let out = run.output.as_ref().expect("the run completes");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "level 1 done.");

    assert_eq!(
        kit.participants().admitted(),
        ["kit-1-0", "kit-2-0", "kit-3-0", "kit-4-0"],
        "the fifth level was never admitted, so never spawned"
    );
    assert!(kit.sse.requests_for("kit/5").is_empty());
    let four = kit.ndjson.requests_for("kit/4");
    assert_eq!(four.len(), 2);
    assert!(
        carries(&four[1], "Error: invoke_agent: too_deep: depth 5 > max 4"),
        "kit-4's model reads the refusal: {}",
        four[1].json()
    );
    assert_eq!(
        edges(&kit)[0],
        row("refusal", "kit-4-0", "kit-5", "too_deep: depth 5 > max 4")
    );
    assert_eq!(kit.participants().open_runs(), 0, "every run released");
}

/// DP-1's rule at call time: a worker whose spec lists only `kit-b` calls
/// `kit-c`, which is on its roster. The broker refuses it with
/// `not_listed` before `kit-c` is spawned.
#[tokio::test]
async fn may_call_enforced_at_broker() {
    let mut a = AgentSpec::named("kit-a");
    a.swarm.delegates_to = vec!["kit-b".to_string()];
    let kit = SwarmKit::start(
        vec![
            AgentDef::from_spec(a, "kit/a", Endpoint::Sse),
            AgentDef::new("kit-b", "kit/b", Endpoint::Ndjson),
            AgentDef::new("kit-c", "kit/c", Endpoint::Ndjson),
        ],
        Script::new().route(
            "kit/a",
            [
                invoke("kit-c", "Do it."),
                Reply::text("kit-c is not mine to call."),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("Start.").await;
    let out = run.output.as_ref().expect("the run completes");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "kit-c is not mine to call.");

    assert_eq!(kit.participants().admitted(), ["kit-a-0"]);
    assert!(kit.ndjson.requests_for("kit/c").is_empty());
    let a = kit.sse.requests_for("kit/a");
    assert_eq!(a.len(), 2);
    assert!(
        carries(
            &a[1],
            "Error: invoke_agent: not_listed: kit-a may not call kit-c"
        ),
        "kit-a's model reads the refusal: {}",
        a[1].json()
    );
    assert_eq!(
        edges(&kit),
        [
            row(
                "refusal",
                "kit-a-0",
                "kit-c",
                "not_listed: kit-a may not call kit-c"
            ),
            row("task", "root", "kit-a-0", "completed"),
        ]
    );
}

/// The `--team audit` run (AGE-745): a leader that lists `kit-analyst` and
/// `kit-writer` calls `kit-checker`, which is on the roster. The broker's
/// root runs as that leader, so the call is refused with `not_listed`
/// before `kit-checker` is spawned, and writes one refusal row.
#[tokio::test]
async fn team_leader_may_call_enforced() {
    let mut lead = AgentSpec::named("kit-lead");
    lead.swarm.delegates_to = vec!["kit-analyst".to_string(), "kit-writer".to_string()];
    let kit = SwarmKit::start_led(
        lead,
        vec![
            AgentDef::new("kit-analyst", "kit/analyst", Endpoint::Sse),
            AgentDef::new("kit-writer", "kit/writer", Endpoint::Ndjson),
            AgentDef::new("kit-checker", "kit/checker", Endpoint::Ndjson),
        ],
        Script::new(),
        Script::new(),
    )
    .await;

    let run = kit.run_leader_to("kit-checker", "Check it.").await;
    let refused = format!("{:?}", run.output);
    assert!(
        refused.contains("not_listed: kit-lead may not call kit-checker"),
        "the leader reads the refusal: {refused}"
    );

    assert!(kit.participants().admitted().is_empty(), "nothing spawned");
    assert!(run.requests.is_empty(), "no model was asked");
    assert_eq!(
        edges(&kit),
        [row(
            "refusal",
            "root",
            "kit-checker",
            "not_listed: kit-lead may not call kit-checker"
        )]
    );
}

/// A plain root, one that runs as no spec, keeps calling any agent on the
/// roster: its own tools decide whether it delegates at all (AGE-745).
#[tokio::test]
async fn root_without_spec_unchanged() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new("kit-analyst", "kit/analyst", Endpoint::Sse),
            AgentDef::new("kit-checker", "kit/checker", Endpoint::Ndjson),
        ],
        Script::new(),
        Script::new().route("kit/checker", [Reply::text("Checked.")]),
    )
    .await;

    let run = kit.run_leader_to("kit-checker", "Check it.").await;
    let out = run.output.as_ref().expect("the call completes");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "Checked.");
    assert_eq!(
        edges(&kit),
        [row("task", "root", "kit-checker-0", "completed")]
    );
}
