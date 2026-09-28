//! AGE-744: the desktop's root conversation delegates through its own
//! broker. The broker is the one the desktop builds — a `ProtocolGateway`
//! with a `LocalRunner` per agent, started by the same
//! [`lazy_gateway_broker::start`] the module-settings controller calls,
//! behind a [`LazyGatewayBroker`] answered the way that controller answers
//! it. The conversation is a real `AgentSession` on the fake model. Only
//! the worker is a stand-in: a short `sh` script that speaks the
//! participant protocol on descriptor 3.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chatty_core::factories::{AgentBuildContext, AgentServices};
use chatty_core::services::StreamSurface;
use chatty_core::session::{AgentSession, AgentSessionConfig, Delegation, SessionEvent, TurnInput};
use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{
    LocalRunner, PARTICIPANT_FD, ParticipantFrame, encode_frame,
};
use chatty_protocol_gateway::worker::TaskMapper;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};

use super::lazy_gateway_broker::{self, LazyGatewayBroker};

const ANALYST: &str = "analyst";
const ROOT_MODEL: &str = "desktop/root";
const ANSWER: &str = "Benford holds: no deviation.";
/// Long enough for a loaded CI box; a hang fails, it does not stall.
const DEADLINE: Duration = Duration::from_secs(60);

/// The gateway's module registry runs no module here.
struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("no module runs in this test".to_string())
    }
}

/// A worker that answers its one task with `ANSWER`, as a real one
/// replaying that turn through `TaskMapper` would.
fn answering_worker(dir: &Path) -> PathBuf {
    const TASK_ID: &str = "@TASK_ID@";
    let mut mapper = TaskMapper::new(TASK_ID);
    let mut frames: Vec<ParticipantFrame> = [
        SessionEvent::TurnStarted,
        SessionEvent::Text(ANSWER.to_string()),
        SessionEvent::TurnEnded,
    ]
    .iter()
    .filter_map(|event| mapper.map(event))
    .collect();
    frames.push(mapper.terminal());
    let frames: String = frames
        .iter()
        .map(|frame| encode_frame(frame).expect("a frame encodes") + "\n")
        .collect();
    std::fs::write(dir.join("frames.jsonl"), frames).expect("frames written");
    let fd = PARTICIPANT_FD;
    worker_script(
        dir,
        &format!(
            r#"here="$(dirname "$0")"
printf '{{"v":2,"type":"hello","card":{{"name":"stand-in"}}}}\n' >&{fd}
read -r welcome <&{fd}
read -r task <&{fd}
id=$(printf '%s' "$task" | sed 's/.*"taskId":"\([^"]*\)".*/\1/')
sed "s/{TASK_ID}/$id/g" "$here/frames.jsonl" >&{fd}
exec sleep 30"#
        ),
    )
}

/// A worker that exits before it ever answers.
fn exiting_worker(dir: &Path) -> PathBuf {
    worker_script(dir, "exit 3")
}

fn worker_script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("chatty-tui");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("the worker is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the worker is executable");
    path
}

/// The desktop's lazy broker over a gateway serving `ANALYST` with
/// `worker`, started on the first request exactly as
/// `module_settings_controller::refresh_runtime` starts it.
fn desktop_broker(worker: PathBuf) -> Arc<LazyGatewayBroker> {
    let (request_tx, mut request_rx) =
        tokio::sync::mpsc::unbounded_channel::<lazy_gateway_broker::StartReply>();
    tokio::spawn(async move {
        let Some(reply) = request_rx.recv().await else {
            return;
        };
        let registry = ModuleRegistry::new(Arc::new(NoopProvider), ResourceLimits::default())
            .expect("the module registry builds");
        let mut gateway = ProtocolGateway::new(Arc::new(tokio::sync::RwLock::new(registry)), 0);
        let runner = LocalRunner::new(worker, gateway.participants()).with_agent_name(ANALYST);
        gateway = gateway.with_virtual_agent(Arc::new(runner));
        let started = lazy_gateway_broker::start(&mut gateway, 0)
            .await
            .map_err(|e| e.to_string());
        let _ = reply.send(started);
        // The gateway serves for as long as the test runs.
        std::future::pending::<()>().await;
        drop(gateway);
    });
    Arc::new(LazyGatewayBroker::new(request_tx))
}

/// A desktop conversation on the fake model at `daemon`, delegating through
/// `broker`, as `app_controller` builds one (its lazy broker and roster).
async fn desktop_session(
    daemon: &FakeDaemon,
    broker: Arc<LazyGatewayBroker>,
    workspace: &Path,
) -> AgentSession {
    let _ = chatty_core::init_repositories();
    let settings = ExecutionSettingsModel {
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings.clone(),
        surface: StreamSurface::Desktop,
        loop_guard: false,
    });
    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::OpenRouter)
        .with_api_key("age-744-key".to_string())
        .with_base_url(daemon.base_url());
    let model = ModelConfig::new(
        ROOT_MODEL.to_string(),
        ROOT_MODEL.to_string(),
        ProviderType::OpenRouter,
        ROOT_MODEL.to_string(),
    );
    session
        .create_conversation(
            "age-744".to_string(),
            "New Chat".to_string(),
            &model,
            &provider,
            AgentBuildContext::from_services(AgentServices {
                exec_settings: Some(settings),
                lazy_broker: Some(broker),
                local_agents: vec![ANALYST.to_string()],
                ..AgentServices::default()
            }),
        )
        .await
        .expect("the desktop conversation builds");
    session
}

/// Run one turn to its end, within [`DEADLINE`]; every event it emitted.
async fn run_turn(session: &mut AgentSession, input: TurnInput) -> Vec<SessionEvent> {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(input, move |event| sink.borrow_mut().push(event))
        .expect("the turn starts");
    tokio::time::timeout(DEADLINE, turn)
        .await
        .expect("the turn ends before the deadline");
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    session.finish_turn(None, vec![]);
    events
}

fn finished(events: &[SessionEvent]) -> Vec<(bool, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Delegation(InvokeAgentProgress::Finished {
                success, result, ..
            }) => Some((*success, result.clone())),
            _ => None,
        })
        .collect()
}

/// The model's own `invoke_agent` in a desktop conversation reaches the
/// in-process broker. Before AGE-744 the desktop's lazy broker offered no
/// direct handle, and every local delegation failed with "needs a broker
/// connection".
#[tokio::test]
async fn desktop_root_invoke_agent_reaches_broker() {
    let dir = tempfile::tempdir().unwrap();
    let broker = desktop_broker(answering_worker(dir.path()));
    let daemon = FakeDaemon::scripted(Script::new().route(
        ROOT_MODEL,
        [
            Reply::tool_call(
                "invoke_agent",
                serde_json::json!({ "agent": ANALYST, "prompt": "check the ledger" }),
            ),
            Reply::text("The analyst found nothing."),
        ],
    ));
    let mut session = desktop_session(&daemon, broker, dir.path()).await;

    let events = run_turn(&mut session, TurnInput::text("audit this")).await;

    let results: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolCallResult { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "one delegation: {events:#?}");
    assert!(
        results[0].contains(ANSWER),
        "the worker's answer came back through the broker: {}",
        results[0]
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SessionEvent::ToolCallError { error, .. } if error.contains("broker connection")
        )),
        "{events:#?}"
    );
    assert_eq!(finished(&events), vec![(true, Some(ANSWER.to_string()))]);
}

/// `/agent <name> <prompt>` runs through the same broker, and a worker that
/// exits without answering ends the delegation row with its error, and the
/// turn with text that says so: it never hangs.
#[tokio::test]
async fn child_exit_ends_delegation_row() {
    let dir = tempfile::tempdir().unwrap();
    let broker = desktop_broker(exiting_worker(dir.path()));
    // The model is never asked: an empty script fails any request loudly.
    let daemon = FakeDaemon::scripted(Script::new());
    let mut session = desktop_session(&daemon, broker, dir.path()).await;

    let events = run_turn(
        &mut session,
        TurnInput::delegation(Delegation {
            agent: ANALYST.to_string(),
            prompt: "check the ledger".to_string(),
        }),
    )
    .await;

    assert!(daemon.requests().is_empty(), "/agent asks no model");
    let ended_with_error = events.iter().any(|event| {
        matches!(event, SessionEvent::ToolCallError { .. })
            || matches!(
                event,
                SessionEvent::ToolCallResult { result, .. } if result.contains("\"success\":false")
            )
    });
    assert!(
        ended_with_error,
        "the row ends with the failure: {events:#?}"
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains(ANALYST), "the turn says what failed: {text}");
    assert!(matches!(events.last(), Some(SessionEvent::TurnEnded)));
    let conversation = session.conversation().expect("the conversation");
    assert_eq!(
        conversation.messages().len(),
        2,
        "the command and its outcome are in history"
    );
}
