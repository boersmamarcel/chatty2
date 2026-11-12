//! EV-1 (AGE-668): the `analyst-swarm` preset end to end on the swarm kit,
//! as `chatty-tui --team analyst-swarm` runs it.
//!
//! The root is a real `AgentSession` running as the preset's coordinator
//! (`AgentBuildContext::from_spec`), and the kit's broker checks its calls
//! against that spec. Its three members are real `chatty-tui` workers on
//! the preset's own specs. The fake model scripts every agent's turns, the
//! benford tool calls included; the `benford` plugin itself runs for real,
//! loaded from the staged `target/wasm-fixtures` (`make wasm-modules`), on
//! the fixture ledger. Nothing leaves loopback.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_core::factories::{AgentBuildContext, AgentServices};
use chatty_core::services::StreamSurface;
use chatty_core::services::swarm_trace::SwarmTrace;
use chatty_core::services::team::load_team;
use chatty_core::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{RecordedRequest, Reply, Script};
use chatty_fabric::{EdgeKind, EdgeRow};
use serde_json::{Value, json};

use super::swarm_kit::{AgentDef, Endpoint, StartedBroker, SwarmKit};

const TEAM: &str = "analyst-swarm";
const DATA_CODER: &str = "analyst-swarm-data-coder";
const BENFORD: &str = "analyst-swarm-benford-analyst";
const REVIEWER: &str = "analyst-swarm-reviewer";

/// Each agent's model, its routing key on the fake server.
const COORDINATOR_MODEL: &str = "kit/analyst-coordinator";
const DATA_CODER_MODEL: &str = "kit/analyst-data-coder";
const BENFORD_MODEL: &str = "kit/analyst-benford";
const REVIEWER_MODEL: &str = "kit/analyst-reviewer";

const TASK: &str = "Audit the amount column of ledger.csv against Benford's Law.";
const LEDGER: &str = include_str!("../../../chatty-core/teams/analyst-swarm/fixture/ledger.csv");
const EXPECTED: &str =
    include_str!("../../../chatty-core/teams/analyst-swarm/fixture/expected.json");

/// The issue's ceiling on one run.
const WALL_TIME: Duration = Duration::from_secs(90);

/// `target/wasm-fixtures`: every staged module, each a directory with its
/// own `module.toml`.
fn module_dir() -> std::path::PathBuf {
    chatty_wasm_runtime::test_support::fixture_path("benford")
        .parent()
        .and_then(std::path::Path::parent)
        .expect("fixtures live in target/wasm-fixtures/<name>/")
        .to_path_buf()
}

/// The amount column cleaned as the data coder is told to: whitespace and
/// `$` stripped, a value that is not a number dropped. What its scripted
/// answer hands on, so the audit runs on exactly the fixture's numbers.
fn clean_amounts() -> (usize, Vec<f64>) {
    let rows: Vec<&str> = LEDGER.lines().skip(1).collect();
    let amounts = rows
        .iter()
        .filter_map(|row| row.rsplit(',').next())
        .filter_map(|amount| amount.trim().trim_start_matches('$').parse().ok())
        .collect();
    (rows.len(), amounts)
}

/// The tool results a model request hands back, parsed as JSON where they
/// are JSON (the benford tools and `list_agents` answer JSON).
fn tool_results(request: &RecordedRequest) -> Vec<Value> {
    request.json()["messages"]
        .as_array()
        .expect("a chat request has messages")
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| message["content"].as_str())
        .filter_map(|content| serde_json::from_str(content).ok())
        .collect()
}

/// The root conversation, running as the preset's coordinator on the kit's
/// SSE endpoint, delegating through the kit's broker.
async fn coordinator_session(
    kit: &SwarmKit,
    coordinator: &chatty_core::agent_spec::AgentSpec,
    members: &[String],
) -> AgentSession {
    let _ = chatty_core::init_repositories();
    let settings = ExecutionSettingsModel {
        workspace_dir: Some(kit.workspace().to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings.clone(),
        surface: StreamSurface::Desktop,
        loop_guard: false,
    });
    let provider = ProviderConfig::new("Fake SSE".to_string(), ProviderType::OpenRouter)
        .with_api_key("swarm-kit-key".to_string())
        .with_base_url(kit.sse.base_url());
    let model = ModelConfig::new(
        COORDINATOR_MODEL.to_string(),
        COORDINATOR_MODEL.to_string(),
        ProviderType::OpenRouter,
        COORDINATOR_MODEL.to_string(),
    );
    let build = AgentBuildContext::from_spec(
        coordinator,
        AgentServices {
            exec_settings: Some(settings),
            lazy_broker: Some(Arc::new(StartedBroker(kit.broker().transport()))),
            local_agents: members.to_vec(),
            ..AgentServices::default()
        },
    )
    .expect("the coordinator's spec builds");
    session
        .create_conversation(
            "age-668".to_string(),
            "New Chat".to_string(),
            &model,
            &provider,
            build.context,
        )
        .await
        .expect("the root conversation builds");
    session
}

/// The root's tool calls in the order it made them: `(name, arguments,
/// result)`.
fn root_tool_calls(events: &[SessionEvent]) -> Vec<(String, Value, String)> {
    let mut calls: Vec<(String, String, Value, String)> = Vec::new();
    for event in events {
        match event {
            SessionEvent::ToolCallStarted { id, name } => {
                calls.push((id.clone(), name.clone(), Value::Null, String::new()));
            }
            SessionEvent::ToolCallInput { id, arguments } => {
                if let Some(call) = calls.iter_mut().find(|call| &call.0 == id) {
                    call.2 = serde_json::from_str(arguments).unwrap_or(Value::Null);
                }
            }
            SessionEvent::ToolCallResult { id, result } => {
                if let Some(call) = calls.iter_mut().find(|call| &call.0 == id) {
                    call.3 = result.clone();
                }
            }
            _ => {}
        }
    }
    calls
        .into_iter()
        .map(|(_, name, arguments, result)| (name, arguments, result))
        .collect()
}

#[tokio::test]
async fn analyst_swarm_smoke() {
    let started = Instant::now();
    let expected: Value = serde_json::from_str(EXPECTED).expect("expected.json parses");
    let (rows, amounts) = clean_amounts();
    assert_eq!(rows as u64, expected["rows"], "the fixture's row count");
    assert_eq!(
        (rows - amounts.len()) as u64,
        expected["rows_without_an_amount"]
    );

    let team = load_team(TEAM, None, None).expect("the preset loads");
    let members = team.agent_names();
    assert_eq!(members, [DATA_CODER, BENFORD, REVIEWER]);
    let spec = |name: &str| {
        team.agents
            .iter()
            .find(|spec| spec.agent.name == name)
            .expect("a member of the preset")
            .clone()
    };
    let roster = vec![
        AgentDef::from_spec(spec(DATA_CODER), DATA_CODER_MODEL, Endpoint::Ndjson),
        AgentDef::from_spec(spec(BENFORD), BENFORD_MODEL, Endpoint::Sse),
        AgentDef::from_spec(spec(REVIEWER), REVIEWER_MODEL, Endpoint::Ndjson),
    ];

    let cleaned = format!(
        "Clean amounts: {}. The file has {rows} rows; dropped INV-1012 and INV-1033 (n/a).",
        json!(amounts)
    );
    let counts = expected["observed_counts"].clone();
    let total = expected["total_analyzed"].clone();
    let verdict = format!(
        "Risk level: {} (chi-square {}, {} degrees of freedom), most deviant digit {}.",
        expected["risk_level"].as_str().unwrap(),
        expected["chi_square"],
        expected["degrees_of_freedom"],
        expected["most_deviant_digit"]
    );
    let sse = Script::new()
        .route(
            COORDINATOR_MODEL,
            [
                Reply::tool_call("list_agents", json!({})),
                Reply::tool_call(
                    "invoke_agent",
                    json!({
                        "agent": DATA_CODER,
                        "prompt": "Clean the amount column of ledger.csv.",
                    }),
                ),
                Reply::tool_call(
                    "invoke_agent",
                    json!({ "agent": BENFORD, "prompt": format!("Audit these amounts: {}", json!(amounts)) }),
                ),
                Reply::tool_call(
                    "invoke_agent",
                    json!({
                        "agent": REVIEWER,
                        "prompt": format!("Review this audit. Cleaning: {cleaned} Verdict: {verdict}"),
                    }),
                ),
                Reply::text(format!(
                    "{verdict} Reviewer: APPROVE. Dropped INV-1012 and INV-1033 (n/a)."
                )),
            ],
        )
        .route(
            BENFORD_MODEL,
            [
                Reply::tool_call(
                    "benford__compute_benford_distribution",
                    json!({ "numbers": amounts }),
                ),
                Reply::tool_call(
                    "benford__chi_square_test",
                    json!({ "observed_counts": counts, "total": total }),
                ),
                Reply::text(verdict.clone()),
            ],
        );
    let ndjson = Script::new()
        .route(
            DATA_CODER_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "ledger.csv" })),
                Reply::text(cleaned.clone()),
            ],
        )
        .route(
            REVIEWER_MODEL,
            [Reply::text(
                "APPROVE\n1. The cleaning kept every row with a number.\n\
                 2. The verdict follows from the plugin's chi-square.",
            )],
        );
    let kit =
        SwarmKit::start_led_with_modules(team.leader.clone(), module_dir(), 2, roster, sse, ndjson)
            .await;
    std::fs::write(kit.workspace().join("ledger.csv"), LEDGER).expect("the ledger is staged");

    let mut session = coordinator_session(&kit, &team.leader, &members).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(TASK), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the coordinator's turn starts");
    tokio::time::timeout(WALL_TIME, turn)
        .await
        .expect("the swarm finishes before the wall-time ceiling");
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    let edges: Vec<EdgeRow> = std::fs::read_to_string(kit.broker().edge_log_path())
        .expect("the edge log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an edge row"))
        .collect();

    // The verdict is the benford plugin's own, on the fixture's numbers.
    let benford = kit.sse.requests_for(BENFORD_MODEL);
    assert_eq!(benford.len(), 3, "two tool calls, then the verdict");
    let distribution = tool_results(&benford[1]);
    assert_eq!(distribution.len(), 1, "{distribution:?}");
    assert_eq!(
        distribution[0]["total_analyzed"],
        expected["total_analyzed"]
    );
    assert_eq!(
        distribution[0]["observed_counts"],
        expected["observed_counts"]
    );
    let results = tool_results(&benford[2]);
    let chi_square = results
        .iter()
        .find(|result| result.get("risk_level").is_some())
        .unwrap_or_else(|| panic!("no chi-square result in {results:?}"));
    for field in [
        "risk_level",
        "chi_square",
        "degrees_of_freedom",
        "most_deviant_digit",
    ] {
        assert_eq!(chi_square[field], expected[field], "{field}: {chi_square}");
    }

    // Each of the four agents was called at least once, each member by the
    // coordinator, and every call completed.
    for (model, requests) in [
        (COORDINATOR_MODEL, kit.sse.requests_for(COORDINATOR_MODEL)),
        (DATA_CODER_MODEL, kit.ndjson.requests_for(DATA_CODER_MODEL)),
        (BENFORD_MODEL, benford),
        (REVIEWER_MODEL, kit.ndjson.requests_for(REVIEWER_MODEL)),
    ] {
        assert!(!requests.is_empty(), "{model} was never asked");
    }
    for member in &members {
        let node = format!("{member}-0");
        assert!(
            edges.iter().any(|row| row.kind == EdgeKind::Task
                && row.from == "root"
                && row.to == node
                && row.outcome == "completed"),
            "root → {node}: {edges:?}"
        );
    }

    // No call was refused, for a policy reason or any other.
    let refused: Vec<&EdgeRow> = edges
        .iter()
        .filter(|row| row.kind == EdgeKind::Refusal || row.outcome.starts_with("refused"))
        .collect();
    assert!(refused.is_empty(), "{refused:?}");

    // The trace is four nodes: the coordinator, at the root, and its three.
    let trace = SwarmTrace::from_edges(&edges, &events);
    let tree = trace.tree();
    assert_eq!(tree.len(), 4, "{tree:#?}");
    let root = tree.root();
    for member in &members {
        let node = trace
            .node_named(&format!("{member}-0"))
            .unwrap_or_else(|| panic!("{member} in {tree:#?}"));
        assert_eq!(tree.parent(node), Some(root), "{member} is the root's");
    }

    // Every agent the coordinator called, it had first been handed by
    // `list_agents`.
    let calls = root_tool_calls(&events);
    let (name, _, listing) = calls.first().expect("the coordinator made tool calls");
    assert_eq!(name, "list_agents", "{calls:?}");
    let listing: Value = serde_json::from_str(listing).expect("list_agents answers JSON");
    let listed: Vec<&str> = listing["agents"]
        .as_array()
        .expect("a list of agents")
        .iter()
        .filter_map(|agent| agent["name"].as_str())
        .collect();
    let called: Vec<&str> = calls
        .iter()
        .filter(|(name, _, _)| name == "invoke_agent")
        .filter_map(|(_, arguments, _)| arguments["agent"].as_str())
        .collect();
    assert_eq!(called, [DATA_CODER, BENFORD, REVIEWER]);
    for agent in &called {
        assert!(listed.contains(agent), "{agent} not in {listed:?}");
    }

    let wall = started.elapsed();
    eprintln!("analyst_swarm_smoke: wall time {wall:?}");
    assert!(wall < WALL_TIME, "{wall:?}");
}

/// The root's name in the cycle variant: a test-only spec that may call the
/// preset's coordinator. The call chain starts at `root` (the broker's name
/// for the session, whatever spec it runs as), so a cycle back to the
/// coordinator needs the coordinator to be a node on the chain first.
const CYCLE_ROOT: &str = "analyst-swarm-cycle-root";
const COORDINATOR: &str = "analyst-swarm-coordinator";
/// The coordinator as a node, below the cycle variant's root.
const COORDINATOR_NODE_MODEL: &str = "kit/analyst-coordinator-node";

/// EV-2 (AGE-669): the `analyst-swarm` preset with one change, built here
/// and nowhere else: the reviewer's `delegates_to` is the coordinator, and
/// the coordinator is a node (`root → coordinator → reviewer`). The
/// scripted reviewer calls it. The broker checks, in this order: the callee
/// exists, the specs allow the call (`delegation_policy::may_call`:
/// reviewer lists the coordinator, the coordinator is exposed and sets no
/// `callers`, so this passes), then the chain (`CallChain::extend`: the
/// coordinator is already on it, so `cycle`). The refusal comes from the
/// cycle guard, before anything is spawned; the run still completes with
/// the audit's verdict.
#[tokio::test]
async fn analyst_swarm_cycle_guard() {
    let started = Instant::now();
    let expected: Value = serde_json::from_str(EXPECTED).expect("expected.json parses");
    let (rows, amounts) = clean_amounts();

    // The preset's specs, untouched on disk; the variant is made in memory.
    let team = load_team(TEAM, None, None).expect("the preset loads");
    let spec = |name: &str| {
        team.agents
            .iter()
            .find(|spec| spec.agent.name == name)
            .expect("a member of the preset")
            .clone()
    };
    let mut reviewer = spec(REVIEWER);
    assert_eq!(
        reviewer.swarm.callers.as_deref(),
        Some(&[COORDINATOR.to_string()][..])
    );
    reviewer.swarm.delegates_to = vec![COORDINATOR.to_string()];
    let coordinator = team.leader.clone();
    assert_eq!(coordinator.agent.name, COORDINATOR);
    assert!(
        coordinator.swarm.exposed && coordinator.swarm.callers.is_none(),
        "the policy must let the reviewer's call through, so the cycle guard is what refuses it"
    );
    let mut cycle_root = coordinator.clone();
    cycle_root.agent.name = CYCLE_ROOT.to_string();
    cycle_root.swarm.delegates_to = vec![COORDINATOR.to_string()];

    let roster = vec![
        AgentDef::from_spec(coordinator, COORDINATOR_NODE_MODEL, Endpoint::Sse),
        AgentDef::from_spec(spec(DATA_CODER), DATA_CODER_MODEL, Endpoint::Ndjson),
        AgentDef::from_spec(spec(BENFORD), BENFORD_MODEL, Endpoint::Sse),
        AgentDef::from_spec(reviewer, REVIEWER_MODEL, Endpoint::Ndjson),
    ];

    let cleaned = format!(
        "Clean amounts: {}. The file has {rows} rows; dropped INV-1012 and INV-1033 (n/a).",
        json!(amounts)
    );
    let verdict = format!(
        "Risk level: {} (chi-square {}, {} degrees of freedom), most deviant digit {}.",
        expected["risk_level"].as_str().unwrap(),
        expected["chi_square"],
        expected["degrees_of_freedom"],
        expected["most_deviant_digit"]
    );
    let call = |agent: &str, prompt: String| {
        Reply::tool_call("invoke_agent", json!({ "agent": agent, "prompt": prompt }))
    };
    let sse = Script::new()
        .route(
            COORDINATOR_MODEL,
            [
                call(COORDINATOR, TASK.to_string()),
                Reply::text(format!("Coordinator's report: {verdict}")),
            ],
        )
        .route(
            COORDINATOR_NODE_MODEL,
            [
                Reply::tool_call("list_agents", json!({})),
                call(DATA_CODER, "Clean the amount column of ledger.csv.".into()),
                call(BENFORD, format!("Audit these amounts: {}", json!(amounts))),
                call(
                    REVIEWER,
                    format!("Review this audit. Cleaning: {cleaned} Verdict: {verdict}"),
                ),
                Reply::text(format!(
                    "{verdict} Reviewer: APPROVE. Dropped INV-1012 and INV-1033 (n/a)."
                )),
            ],
        )
        .route(
            BENFORD_MODEL,
            [
                Reply::tool_call(
                    "benford__compute_benford_distribution",
                    json!({ "numbers": amounts }),
                ),
                Reply::tool_call(
                    "benford__chi_square_test",
                    json!({
                        "observed_counts": expected["observed_counts"],
                        "total": expected["total_analyzed"],
                    }),
                ),
                Reply::text(verdict.clone()),
            ],
        );
    let ndjson = Script::new()
        .route(
            DATA_CODER_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "ledger.csv" })),
                Reply::text(cleaned.clone()),
            ],
        )
        .route(
            REVIEWER_MODEL,
            [
                call(COORDINATOR, "Re-run the whole audit.".into()),
                Reply::text("APPROVE\n1. The verdict follows from the plugin's chi-square."),
            ],
        );
    let kit =
        SwarmKit::start_led_with_modules(cycle_root.clone(), module_dir(), 2, roster, sse, ndjson)
            .await;
    std::fs::write(kit.workspace().join("ledger.csv"), LEDGER).expect("the ledger is staged");

    let mut session = coordinator_session(&kit, &cycle_root, &[COORDINATOR.to_string()]).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(TASK), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the root's turn starts");
    tokio::time::timeout(WALL_TIME, turn)
        .await
        .expect("the swarm finishes before the wall-time ceiling");
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    let edges: Vec<EdgeRow> = std::fs::read_to_string(kit.broker().edge_log_path())
        .expect("the edge log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("an edge row"))
        .collect();

    // The reviewer's call is refused with `cycle`, the chain named, and its
    // model reads the refusal as the tool result.
    let chain = "root \u{2192} analyst-swarm-coordinator \u{2192} analyst-swarm-reviewer \u{2192} analyst-swarm-coordinator";
    let reviewer_requests = kit.ndjson.requests_for(REVIEWER_MODEL);
    assert_eq!(reviewer_requests.len(), 2, "the call, then the answer");
    assert!(
        reviewer_requests[1]
            .json()
            .to_string()
            .contains(&format!("Error: invoke_agent: cycle: {chain}")),
        "{}",
        reviewer_requests[1].json()
    );

    // Nothing was spawned for it: one coordinator node and one of each
    // member were admitted, and the coordinator's model was asked only by
    // that one node (list, three calls, the answer).
    let mut admitted = kit.participants().admitted();
    admitted.sort();
    assert_eq!(
        admitted,
        [
            format!("{BENFORD}-0"),
            format!("{COORDINATOR}-0"),
            format!("{DATA_CODER}-0"),
            format!("{REVIEWER}-0"),
        ]
    );
    assert_eq!(kit.sse.requests_for(COORDINATOR_NODE_MODEL).len(), 5);

    // The edge log has the refusal row, and it is the only refusal.
    let refusals: Vec<&EdgeRow> = edges
        .iter()
        .filter(|row| row.kind == EdgeKind::Refusal || row.outcome.starts_with("refused"))
        .collect();
    assert_eq!(refusals.len(), 1, "{edges:?}");
    let refusal = refusals[0];
    assert_eq!(refusal.kind, EdgeKind::Refusal, "{refusal:?}");
    assert_eq!(refusal.from, format!("{REVIEWER}-0"));
    assert_eq!(refusal.to, COORDINATOR);
    assert_eq!(refusal.outcome, format!("cycle: {chain}"));

    // The run completes with the right verdict: the plugin's own, on the
    // fixture's numbers, reaching the root through the coordinator.
    let benford = kit.sse.requests_for(BENFORD_MODEL);
    assert_eq!(benford.len(), 3, "two tool calls, then the verdict");
    let results = tool_results(&benford[2]);
    let chi_square = results
        .iter()
        .find(|result| result.get("risk_level").is_some())
        .unwrap_or_else(|| panic!("no chi-square result in {results:?}"));
    for field in [
        "risk_level",
        "chi_square",
        "degrees_of_freedom",
        "most_deviant_digit",
    ] {
        assert_eq!(chi_square[field], expected[field], "{field}: {chi_square}");
    }
    for member in [COORDINATOR, DATA_CODER, BENFORD, REVIEWER] {
        assert!(
            edges.iter().any(|row| row.kind == EdgeKind::Task
                && row.to == format!("{member}-0")
                && row.outcome == "completed"),
            "{member}-0 completed: {edges:?}"
        );
    }
    let calls = root_tool_calls(&events);
    let (_, _, report) = calls
        .iter()
        .find(|(name, _, _)| name == "invoke_agent")
        .expect("the root called the coordinator");
    assert!(report.contains(&verdict), "{report}");
    let said: String = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(said.contains(&verdict), "{said}");
    assert_eq!(kit.participants().open_runs(), 0, "every run released");

    let wall = started.elapsed();
    eprintln!("analyst_swarm_cycle_guard: wall time {wall:?}");
    assert!(wall < WALL_TIME, "{wall:?}");
}
