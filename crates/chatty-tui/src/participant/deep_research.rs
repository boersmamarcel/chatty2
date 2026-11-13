//! AGE-910: the `deep-research` preset end to end on the swarm kit, as
//! `chatty-tui --team deep-research` runs it.
//!
//! The root is a real `AgentSession` running as the preset's lead
//! (`AgentBuildContext::from_spec`): its `[swarm.rounds]` hook, its
//! `write_file` and its parallel `invoke_agent` are the real ones. The
//! researchers, the writer and the checker are real `chatty-tui` workers on
//! the preset's own specs, each researcher's note checked against the
//! team's schema (TD-2). The fake model scripts every turn. The workers
//! have their internet tools (`fetch_enabled`), but a scripted fetch names
//! a loopback URL, which the fetch tool's SSRF guard refuses before any
//! request: nothing leaves the machine. When the turn ends, the test
//! finishes the report as the host does (`team_report::finish_file`), on
//! the notes the host kept in the workspace (`research_notes`, AGE-1295).
//! The lead's own results carry only a reference to each note.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_core::agent_spec::AgentSpec;
use chatty_core::factories::{AgentBuildContext, AgentServices};
use chatty_core::services::StreamSurface;
use chatty_core::services::budget_stop::BUDGET_EXHAUSTED;
use chatty_core::services::research_notes::{NOTE_FILES_HEADING, read_notes};
use chatty_core::services::round_limit::ROUND_LIMIT_REACHED;
use chatty_core::services::swarm_trace::SwarmTrace;
use chatty_core::services::team::{Team, load_team};
use chatty_core::services::team_report::{
    CITATION_CHECK_HEADING, COST_HEADING, PARTIAL_HEADING, RunTally, answer_from_report,
    citations_outside_notes, finish_file, note_in_result, note_urls,
};
use chatty_core::session::{AgentSession, AgentSessionConfig, SessionEvent, TurnInput};
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{Reply, Script};
use chatty_fabric::{EdgeKind, EdgeRow};
use serde_json::{Value, json};

use super::swarm_kit::{AgentDef, Endpoint, StartedBroker, SwarmKit};

const TEAM: &str = "deep-research";
const RESEARCHER: &str = "deep-research-web-researcher";
const WRITER: &str = "deep-research-writer";
const CHECKER: &str = "deep-research-checker";

/// Each agent's model, its routing key on the fake server. Researchers are
/// told apart by their brief (`Brief <id>`), one body route each.
const LEAD_MODEL: &str = "kit/research-lead";
const RESEARCHER_MODEL: &str = "kit/research-researcher";
const WRITER_MODEL: &str = "kit/research-writer";
const CHECKER_MODEL: &str = "kit/research-checker";

const QUESTION: &str = "How tall is the Eiffel Tower, and how did its height change?";
const PLAN: &str = "research-plan.md";
const REPORT: &str = "research-report.md";
/// Where the lead's spec has the host keep the notes (AGE-1295).
const NOTES_DIR: &str = "research-notes";

/// Generous for CI under load; a run takes seconds.
const WALL_TIME: Duration = Duration::from_secs(120);

/// A note as a researcher hands it back: `claims` are `(url, quote)`.
fn note(id: &str, claims: &[(&str, &str)]) -> Value {
    json!({
        "sub_question": format!("{id}: the part of the question it covers"),
        "claims": claims.iter().map(|(url, quote)| json!({
            "claim": format!("A fact for {id}."),
            "url": url,
            "quote": quote,
            "accessed": "2026-11-12",
        })).collect::<Vec<_>>(),
        "open_questions": [format!("What {id} left open")],
    })
}

/// A worker's final answer carrying `handoff` as its one `json` block.
fn handoff(handoff: &Value) -> Reply {
    Reply::text(format!("Done.\n```json\n{handoff}\n```"))
}

/// The lead's call to one researcher with sub-question `id`.
fn research(id: &str) -> (String, Value) {
    (
        "invoke_agent".to_string(),
        json!({
            "agent": RESEARCHER,
            "prompt": format!(
                "Brief {id}. Objective: {id}. Output: your note. Scope: the tower in Paris, \
                 metres. Boundaries: leave the other sub-questions alone."
            ),
        }),
    )
}

fn invoke(agent: &str, prompt: impl Into<String>) -> Reply {
    Reply::tool_call(
        "invoke_agent",
        json!({ "agent": agent, "prompt": prompt.into() }),
    )
}

fn write(path: &str, content: &str) -> Reply {
    Reply::tool_call("write_file", json!({ "path": path, "content": content }))
}

/// The roster with the team's handoff schemas, as `--team` serves it.
fn roster(team: &Team) -> Vec<AgentDef> {
    let spec = |name: &str| {
        team.agents
            .iter()
            .find(|spec| spec.agent.name == name)
            .expect("a member of the preset")
            .clone()
    };
    let schema = |name: &str| {
        team.handoffs[name]
            .schema
            .to_value()
            .expect("the schema is JSON")
    };
    vec![
        AgentDef::from_spec(spec(RESEARCHER), RESEARCHER_MODEL, Endpoint::Ndjson)
            .with_handoff(schema(RESEARCHER)),
        AgentDef::from_spec(spec(WRITER), WRITER_MODEL, Endpoint::Ndjson),
        AgentDef::from_spec(spec(CHECKER), CHECKER_MODEL, Endpoint::Sse)
            .with_handoff(schema(CHECKER)),
    ]
}

/// The root conversation, running as the preset's lead on the kit's SSE
/// endpoint, delegating through the kit's broker. With `window`, the lead
/// is an unattended run (as `--team` is) on a model with that context
/// window, so its history is compacted near it.
async fn lead_session(
    kit: &SwarmKit,
    lead: &AgentSpec,
    members: &[String],
    window: Option<i32>,
) -> AgentSession {
    let _ = chatty_core::init_repositories();
    let settings = ExecutionSettingsModel {
        enabled: true,
        workspace_dir: Some(kit.workspace().to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        approval_mode: ApprovalMode::AutoApproveAll,
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
    let mut model = ModelConfig::new(
        LEAD_MODEL.to_string(),
        LEAD_MODEL.to_string(),
        ProviderType::OpenRouter,
        LEAD_MODEL.to_string(),
    );
    model.max_context_window = window;
    let mut build = AgentBuildContext::from_spec(
        lead,
        AgentServices {
            exec_settings: Some(settings),
            lazy_broker: Some(Arc::new(StartedBroker(kit.broker().transport()))),
            local_agents: members.to_vec(),
            ..AgentServices::default()
        },
    )
    .expect("the lead's spec builds");
    build.context.unattended = window.is_some();
    session
        .create_conversation(
            "age-910".to_string(),
            "New Chat".to_string(),
            &model,
            &provider,
            build.context,
        )
        .await
        .expect("the root conversation builds");
    session
}

/// What one run left behind.
struct Run {
    kit: SwarmKit,
    events: Vec<SessionEvent>,
    edges: Vec<EdgeRow>,
    /// The root's tool calls in order: `(name, arguments, result)`.
    calls: Vec<(String, Value, String)>,
    /// The researchers' notes, as the host kept them in the workspace.
    notes: Vec<Value>,
    /// The references the lead's researcher results carry instead.
    references: Vec<Value>,
    /// What the root said.
    said: String,
    wall: Duration,
}

impl Run {
    /// The report as the host finishes it when the run ends.
    fn finish_report(&self) -> String {
        let trace = SwarmTrace::from_edges(&self.edges, &self.events);
        let tally = RunTally::from_trace(&trace, None, self.wall);
        let path = finish_file(
            &self.kit.workspace(),
            REPORT,
            &self.said,
            &self.notes,
            &tally,
        )
        .expect("the report is finished");
        std::fs::read_to_string(path).expect("the report reads")
    }

    /// Completed tasks from the root to a `name` node.
    fn completed(&self, name: &str) -> usize {
        self.edges
            .iter()
            .filter(|row| {
                row.kind == EdgeKind::Task
                    && row.from == "root"
                    && row.to.starts_with(&format!("{name}-"))
                    && row.outcome == "completed"
            })
            .count()
    }
}

/// Run the lead's turn on `QUESTION` with these scripts.
async fn run(sse: Script, ndjson: Script) -> Run {
    run_in_window(sse, ndjson, None).await
}

/// [`run`], the lead on a model with a `window`-token context when set.
async fn run_in_window(sse: Script, ndjson: Script, window: Option<i32>) -> Run {
    let started = Instant::now();
    let team = load_team(TEAM, None, None).expect("the preset loads");
    let members = team.agent_names();
    let kit =
        SwarmKit::start_led_with_web(team.leader.clone(), 4, roster(&team), sse, ndjson).await;

    let mut session = lead_session(&kit, &team.leader, &members, window).await;
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(QUESTION), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the lead's turn starts");
    tokio::time::timeout(WALL_TIME, turn)
        .await
        .expect("the team finishes before the wall-time ceiling");
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    let edges: Vec<EdgeRow> = std::fs::read_to_string(kit.broker().edge_log_path())
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("an edge row"))
        .collect();
    let calls = root_tool_calls(&events);
    let notes = read_notes(&kit.workspace(), NOTES_DIR);
    let references = calls
        .iter()
        .filter(|(name, args, _)| name == "invoke_agent" && args["agent"] == RESEARCHER)
        .filter_map(|(_, _, result)| {
            let result: Value = serde_json::from_str(result).ok()?;
            Some(result.get("note")?.clone())
        })
        .collect();
    let said = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    Run {
        kit,
        events,
        edges,
        calls,
        notes,
        references,
        said,
        wall: started.elapsed(),
    }
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
            SessionEvent::ToolCallError { id, error } => {
                if let Some(call) = calls.iter_mut().find(|call| &call.0 == id) {
                    call.3 = error.clone();
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

const SQ1_URL: &str = "https://www.toureiffel.paris/en/the-monument/key-figures";
const SQ1_QUOTE: &str = "330 metres";
const SQ2_URL: &str = "https://www.britannica.com/topic/Eiffel-Tower-Paris-France";
const SQ2_QUOTE: &str = "originally 300 metres";

/// A report citing the two notes' URLs, and `extra` besides.
fn report_text(extra: &str) -> String {
    format!(
        "# The Eiffel Tower's height\n\nIt is 330 m tall [1]({SQ1_URL}), up from 300 m \
         [2]({SQ2_URL}).{extra}\n\n## Gaps and disagreements\n\nOpen:\n- What SQ1 left open\n"
    )
}

/// The checker's handoff for the two notes' URLs.
fn check_pass() -> Value {
    json!({
        "verdict": "PASS",
        "citations": [
            { "url": SQ1_URL, "status": "found" },
            { "url": SQ2_URL, "status": "found" },
        ],
        "sampled_claims": [],
    })
}

/// The scripts of one full run: plan, one round of two researchers side by
/// side, the writer (writing `report`), the checker, the answer.
fn full_run_scripts(report: &str) -> (Script, Script) {
    let notes = json!([
        note("SQ1", &[(SQ1_URL, SQ1_QUOTE)]),
        note("SQ2", &[(SQ2_URL, SQ2_QUOTE)]),
    ]);
    let sse = Script::new()
        .route(
            LEAD_MODEL,
            [
                Reply::tool_call("list_agents", json!({})),
                write(PLAN, "# Plan\n\n- SQ1: height now\n- SQ2: height then\n"),
                Reply::ToolCalls(vec![research("SQ1"), research("SQ2")]),
                invoke(
                    WRITER,
                    format!("Write the report to {REPORT} from these notes: {notes}"),
                ),
                invoke(
                    CHECKER,
                    format!("Check {REPORT} against these notes: {notes}"),
                ),
                Reply::text(format!("The report is at {REPORT}. Checker: PASS.")),
            ],
        )
        .route(
            CHECKER_MODEL,
            [Reply::text(format!("PASS\n```json\n{}\n```", check_pass()))],
        );
    let ndjson = Script::new()
        .route(
            WRITER_MODEL,
            [
                write(REPORT, report),
                Reply::text(format!("{REPORT}: the height, cited.")),
            ],
        )
        .route_body(
            "Brief SQ1",
            [
                // The SSRF guard refuses loopback before any request.
                Reply::tool_call("fetch", json!({ "url": "http://127.0.0.1:9/sq1" })),
                handoff(&note("SQ1", &[(SQ1_URL, SQ1_QUOTE)])),
            ],
        )
        .route_body(
            "Brief SQ2",
            [handoff(&note("SQ2", &[(SQ2_URL, SQ2_QUOTE)]))],
        );
    (sse, ndjson)
}

/// AGE-910: plan, a round of two researchers side by side, each handing
/// back a schema-checked note; the writer's report from the notes; the
/// checker once; the report in the workspace, cited from the notes only
/// and ending with the run's cost.
#[tokio::test]
async fn deep_research_round_trip_produces_cited_report() {
    let (sse, ndjson) = full_run_scripts(&report_text(""));
    let run = run(sse, ndjson).await;

    // The plan is in the workspace, where a compaction cannot lose it.
    let plan = std::fs::read_to_string(run.kit.workspace().join(PLAN)).expect("the plan");
    assert!(plan.contains("SQ1") && plan.contains("SQ2"), "{plan}");

    // Both researchers ran, from one model turn, and each note passed the
    // schema. AGE-1295: the host kept each in its own file of round 1, and
    // the lead's result carries a reference to it, not the note.
    assert_eq!(run.completed(RESEARCHER), 2, "{:?}", run.edges);
    assert_eq!(run.notes.len(), 2, "{:?}", run.calls);
    assert_eq!(run.references.len(), 2, "{:?}", run.calls);
    let mut files: Vec<&str> = run
        .references
        .iter()
        .filter_map(|reference| reference["note_file"].as_str())
        .collect();
    files.sort_unstable();
    assert_eq!(
        files,
        ["research-notes/1-1.json", "research-notes/1-2.json"]
    );
    for (_, args, result) in run
        .calls
        .iter()
        .filter(|(_, args, _)| args["agent"] == RESEARCHER)
    {
        assert!(note_in_result(result).is_none(), "no note in {result}");
        assert!(
            !result.contains(SQ1_QUOTE) && !result.contains(SQ2_QUOTE),
            "{args}: {result}"
        );
    }
    // The writer is offered both files.
    let writer = run.kit.ndjson.requests_for(WRITER_MODEL);
    let brief = writer[0].json().to_string();
    assert!(brief.contains(NOTE_FILES_HEADING), "{brief}");
    for file in &files {
        assert!(brief.contains(file), "{file} offered to the writer");
    }
    let researcher = run.kit.ndjson.requests_for("Brief SQ1");
    assert!(!researcher.is_empty());
    let offered = researcher[0].json()["tools"].to_string();
    for tool in ["\"search_web\"", "\"fetch\""] {
        assert!(
            offered.contains(tool),
            "the researcher is offered {tool}: {offered}"
        );
    }
    for tool in ["\"write_file\"", "\"shell_execute\""] {
        assert!(!offered.contains(tool), "the researcher has no {tool}");
    }

    // The writer, then the checker once; nothing refused.
    assert_eq!(run.completed(WRITER), 1);
    assert_eq!(run.completed(CHECKER), 1);
    assert_eq!(run.kit.sse.requests_for(CHECKER_MODEL).len(), 1);
    assert!(
        !run.edges
            .iter()
            .any(|row| row.kind == EdgeKind::Refusal || row.outcome.starts_with("refused")),
        "{:?}",
        run.edges
    );
    let called: Vec<&str> = run
        .calls
        .iter()
        .filter(|(name, _, _)| name == "invoke_agent")
        .filter_map(|(_, args, _)| args["agent"].as_str())
        .collect();
    assert_eq!(called, [RESEARCHER, RESEARCHER, WRITER, CHECKER]);

    // The report: cited, from the notes only, ending with the cost.
    let report = run.finish_report();
    assert!(
        report.starts_with("# The Eiffel Tower's height"),
        "{report}"
    );
    assert_eq!(
        note_urls(&run.notes).into_iter().collect::<Vec<_>>(),
        [SQ2_URL, SQ1_URL]
    );
    assert!(report.contains(SQ1_URL) && report.contains(SQ2_URL));
    assert!(citations_outside_notes(&report, &run.notes).is_empty());
    assert!(!report.contains(CITATION_CHECK_HEADING), "{report}");
    assert!(report.contains("Gaps and disagreements"));
    // AGE-1341: the writer put the list right after its line; the host
    // finished it with a blank line before, so Markdown renders a list.
    assert!(
        report.contains("Open:\n\n- What SQ1 left open"),
        "a blank line before the list: {report}"
    );
    let footer = &report[report.find(COST_HEADING).expect("the cost footer")..];
    assert!(footer.contains("- Web searches: 0"), "{footer}");
    assert!(footer.contains("- Page fetches: 1"), "{footer}");
    assert!(footer.contains("- Wall time: "), "{footer}");
    assert!(footer.contains("- Spend: "), "{footer}");
    assert!(run.wall < WALL_TIME, "{:?}", run.wall);
}

/// AGE-910: a URL the writer cites from outside the notes is caught by the
/// host's citation check: the finished report lists it, and only it.
#[tokio::test]
async fn writer_cites_only_note_urls() {
    const MADE_UP: &str = "https://made-up.example/eiffel";
    let (sse, ndjson) = full_run_scripts(&report_text(&format!(" It sways 15 cm [3]({MADE_UP}).")));
    let run = run(sse, ndjson).await;

    assert_eq!(run.notes.len(), 2);
    let report = run.finish_report();
    assert_eq!(citations_outside_notes(&report, &run.notes), [MADE_UP]);
    let check = &report[report.find(CITATION_CHECK_HEADING).expect("the check")..];
    let check = &check[..check.find(COST_HEADING).unwrap_or(check.len())];
    assert!(check.contains(&format!("- {MADE_UP}")), "{check}");
    for url in [SQ1_URL, SQ2_URL] {
        assert!(!check.contains(url), "{url} is in a note: {check}");
    }
}

/// AGE-910: the lead's fifth round is refused by `[swarm.rounds]`,
/// whatever its model wants: no fifth researcher starts, the lead reads
/// `round_limit`, and the report is still written from four rounds' notes.
#[tokio::test]
async fn lead_stops_at_round_limit() {
    let team = load_team(TEAM, None, None).expect("the preset loads");
    let max = team.leader.swarm.rounds.as_ref().expect("a round cap").max;
    assert_eq!(max, 4);
    let ids: Vec<String> = (1..=max + 1).map(|round| format!("R{round}")).collect();

    let mut lead: Vec<Reply> = vec![Reply::tool_call("list_agents", json!({}))];
    for id in &ids {
        lead.push(Reply::ToolCalls(vec![research(id)]));
    }
    lead.push(invoke(WRITER, format!("Write the report to {REPORT}.")));
    lead.push(Reply::text(format!("The report is at {REPORT}.")));
    let sse = Script::new().route(LEAD_MODEL, lead);
    let mut ndjson = Script::new().route(
        WRITER_MODEL,
        [
            write(REPORT, &report_text("")),
            Reply::text(format!("{REPORT}: written.")),
        ],
    );
    for (round, id) in ids.iter().enumerate() {
        let url = format!("https://example.org/round-{}", round + 1);
        ndjson = ndjson.route_body(
            format!("Brief {id}"),
            [handoff(&note(id, &[(url.as_str(), "a quote")]))],
        );
    }
    let run = run(sse, ndjson).await;

    // Four rounds ran; the fifth researcher never started.
    assert_eq!(run.completed(RESEARCHER), max as usize, "{:?}", run.edges);
    for (round, id) in ids.iter().enumerate() {
        let asked = run.kit.ndjson.requests_for(&format!("Brief {id}")).len();
        assert_eq!(asked > 0, round < max as usize, "{id} asked {asked} times");
    }
    let refused = run
        .calls
        .iter()
        .find(|(name, args, _)| {
            name == "invoke_agent" && args["prompt"].as_str().is_some_and(|p| p.contains("R5"))
        })
        .expect("the fifth round's call");
    assert!(
        refused.2.starts_with(ROUND_LIMIT_REACHED),
        "the lead reads the limit: {}",
        refused.2
    );
    let lead_requests = run.kit.sse.requests_for(LEAD_MODEL);
    assert!(
        lead_requests.iter().any(|request| request
            .json()
            .to_string()
            .contains("round_limit: all 4 rounds")),
        "the refusal reaches the lead's model"
    );

    // The lead went on to the writer: the report is written.
    assert_eq!(run.completed(WRITER), 1);
    assert_eq!(run.notes.len(), max as usize);
    let report = run.finish_report();
    assert!(!report.starts_with(PARTIAL_HEADING), "{report}");
    assert!(report.contains(COST_HEADING));
}

/// AGE-910 with AGE-903: the endpoint refuses the second round's
/// researcher for a spent budget. The lead stops without another model
/// call or delegation, the writer never runs, and the host keeps a partial
/// report: the first round's note, and the cost so far.
#[tokio::test]
async fn budget_exhausted_mid_run_still_yields_a_partial_report() {
    let sse = Script::new().route(
        LEAD_MODEL,
        [
            Reply::tool_call("list_agents", json!({})),
            Reply::ToolCalls(vec![research("SQ1")]),
            Reply::ToolCalls(vec![research("SQ2")]),
            invoke(WRITER, "unreachable: the budget is spent"),
            Reply::text("unreachable: the budget is spent"),
        ],
    );
    let ndjson = Script::new()
        .route(WRITER_MODEL, [Reply::text("unreachable: the writer ran")])
        .route_body(
            "Brief SQ1",
            [handoff(&note("SQ1", &[(SQ1_URL, SQ1_QUOTE)]))],
        )
        .route_body(
            "Brief SQ2",
            [Reply::ErrorBody(
                402,
                json!({ "error": "cap_exceeded", "month_to_date": 2.5, "cap_usd": 2.0 }),
            )],
        );
    let run = run(sse, ndjson).await;

    assert_eq!(
        run.kit.ndjson.requests_for(WRITER_MODEL).len(),
        0,
        "no writer"
    );
    assert_eq!(
        run.kit.sse.requests_for(LEAD_MODEL).len(),
        3,
        "no lead model call after the stop"
    );
    let stopped = run
        .calls
        .iter()
        .find(|(_, args, _)| {
            args["prompt"]
                .as_str()
                .is_some_and(|p| p.contains("Brief SQ2"))
        })
        .expect("the second round's call");
    assert!(stopped.2.contains(BUDGET_EXHAUSTED), "{}", stopped.2);
    assert!(
        !run.kit.workspace().join(REPORT).exists(),
        "no report was written"
    );

    let report = run.finish_report();
    assert!(report.starts_with(PARTIAL_HEADING), "{report}");
    assert!(report.contains("Budget of $2.00 spent"), "{report}");
    assert!(
        report.contains(SQ1_URL) && report.contains(SQ1_QUOTE),
        "the finished note is kept: {report}"
    );
    assert!(!report.contains("unreachable"), "{report}");
    assert!(report.contains(COST_HEADING), "{report}");
}

/// A long round summary for the plan: enough text that a few of them fill
/// a small window.
fn long_summary(round: usize) -> String {
    let words: String = (0..2_500).map(|i| format!("r{round}w{i} ")).collect();
    format!("\n## Round {round}\n\n{words}\n")
}

/// AGE-1295: a long run whose lead is compacted mid-run, as the first real
/// run was. Three rounds of researchers, each followed by a long round
/// summary the lead appends to the plan, on a small window. The notes do
/// not depend on what the lead's history keeps: each is in its own file,
/// the lead only ever held references, the writer is offered every file
/// and reads each one, the host's citation check reads the files and
/// lists nothing a note carries, and the run's answer opens with the
/// report's own opening.
#[tokio::test]
async fn long_run_with_compaction_keeps_every_note_in_the_workspace() {
    const WINDOW: i32 = 32_000;
    // Round → its sub-questions, each with one URL and quote of its own.
    let rounds: [&[&str]; 3] = [&["QA1", "QB1"], &["QA2", "QB2"], &["QA3"]];
    let source = |id: &str| {
        (
            format!("https://source.example/{}", id.to_lowercase()),
            format!("quote for {id}"),
        )
    };
    let ids: Vec<&str> = rounds
        .iter()
        .flat_map(|round| round.iter().copied())
        .collect();

    let mut lead = vec![
        Reply::tool_call("list_agents", json!({})),
        write(PLAN, "# Plan\n\n- QA: one half\n- QB: the other\n"),
    ];
    for (round, members) in rounds.iter().enumerate() {
        lead.push(Reply::ToolCalls(
            members.iter().map(|id| research(id)).collect(),
        ));
        lead.push(Reply::tool_call(
            "write_file",
            json!({ "path": PLAN, "content": long_summary(round + 1), "append": true }),
        ));
    }
    lead.push(invoke(WRITER, format!("Write the report to {REPORT}.")));
    lead.push(invoke(CHECKER, format!("Check {REPORT}.")));
    lead.push(Reply::text("Checker: PASS. No sub-question is left open."));

    let citations: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "url": source(id).0, "status": "found" }))
        .collect();
    let check = json!({ "verdict": "PASS", "citations": citations, "sampled_claims": [] });
    let sse = Script::new()
        // The compaction's summary call runs on the lead's own model.
        .route_body(
            "You are compacting the working history",
            (0..8).map(|_| Reply::text("Done so far: rounds of research. Next step: go on.")),
        )
        .route(LEAD_MODEL, lead)
        .route(
            CHECKER_MODEL,
            [Reply::text(format!("PASS\n```json\n{check}\n```"))],
        );

    // Every note file the run will have: round r has one per member.
    let files: Vec<String> = rounds
        .iter()
        .enumerate()
        .flat_map(|(round, members)| {
            (1..=members.len()).map(move |n| format!("{NOTES_DIR}/{}-{n}.json", round + 1))
        })
        .collect();
    let report = format!(
        "# Halves\n\nSources are numbered by note.\n\n## Short answer\n\nBoth halves are \
         answered [1]({}).\n\n## Details\n\n{}\n\n## Gaps and disagreements\n\n- None\n",
        source("QA1").0,
        ids.iter()
            .enumerate()
            .map(|(n, id)| format!("- {id} [{}]({})", n + 1, source(id).0))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut ndjson = Script::new().route(
        WRITER_MODEL,
        [
            Reply::ToolCalls(
                files
                    .iter()
                    .map(|file| ("read_file".to_string(), json!({ "path": file })))
                    .collect(),
            ),
            write(REPORT, &report),
            Reply::text(format!("{REPORT}: written from every note.")),
        ],
    );
    for id in &ids {
        let (url, quote) = source(id);
        ndjson = ndjson.route_body(
            format!("Brief {id}."),
            [handoff(&note(id, &[(url.as_str(), quote.as_str())]))],
        );
    }
    let run = run_in_window(sse, ndjson, Some(WINDOW)).await;

    // The lead's history was compacted mid-run.
    let summaries = run
        .kit
        .sse
        .requests_for("You are compacting the working history")
        .len();
    assert!(summaries >= 1, "the lead's history was compacted");
    assert_eq!(run.completed(RESEARCHER), ids.len(), "{:?}", run.edges);

    // Every note survived, in its file, whole: quote and all.
    assert_eq!(run.notes.len(), ids.len());
    for file in &files {
        assert!(run.kit.workspace().join(file).is_file(), "{file}");
    }
    let kept: Vec<String> = run.notes.iter().map(Value::to_string).collect();
    for id in &ids {
        let (url, quote) = source(id);
        assert!(
            kept.iter()
                .any(|note| note.contains(&url) && note.contains(&quote)),
            "{id}'s note is kept"
        );
    }

    // The lead only ever held references: no quote reached its model.
    assert_eq!(run.references.len(), ids.len());
    for request in run.kit.sse.requests_for(LEAD_MODEL) {
        let body = request.json().to_string();
        for id in &ids {
            assert!(
                !body.contains(&source(id).1),
                "{id}'s quote reached the lead"
            );
        }
    }

    // The writer was offered every note file and read each one.
    let writer = run.kit.ndjson.requests_for(WRITER_MODEL);
    let brief = writer[0].json().to_string();
    assert!(
        brief.contains(&format!("{NOTE_FILES_HEADING} ({} files", files.len())),
        "{brief}"
    );
    for file in &files {
        assert!(
            brief.contains(file.as_str()),
            "{file} offered to the writer"
        );
    }
    let read = writer[1].json().to_string();
    for id in &ids {
        assert!(read.contains(&source(id).1), "the writer read {id}'s note");
    }

    // The citation check reads the files: nothing a note carries is listed.
    let finished = run.finish_report();
    assert!(citations_outside_notes(&finished, &run.notes).is_empty());
    assert!(!finished.contains(CITATION_CHECK_HEADING), "{finished}");

    // The run's answer opens with the report's own opening, read from the
    // file, then the lead's last reply.
    let answer = answer_from_report(
        &finished,
        REPORT,
        "Checker: PASS. No sub-question is left open.",
    )
    .expect("a written report has an opening");
    assert!(
        answer.starts_with(&format!(
            "# Halves\n\n## Short answer\n\nBoth halves are answered [1]({}).",
            source("QA1").0
        )),
        "{answer}"
    );
    assert!(answer.ends_with("Checker: PASS. No sub-question is left open."));
}
