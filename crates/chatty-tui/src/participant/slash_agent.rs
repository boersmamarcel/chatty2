//! AGE-744 on the swarm kit: `/agent <name> <prompt>` is a turn of the
//! root conversation handed to that agent through the root's own broker,
//! so a lead that delegates grows the swarm tree under it, exactly as a
//! model-issued `invoke_agent` does. The root is a real `AgentSession`
//! (the desktop's turn contract); everything under it is real workers on
//! the fake model.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use chatty_core::factories::{AgentBuildContext, AgentServices};
use chatty_core::services::StreamSurface;
use chatty_core::services::swarm_trace::SwarmTrace;
use chatty_core::session::{AgentSession, AgentSessionConfig, Delegation, SessionEvent, TurnInput};
use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{Reply, Script};
use serde_json::json;

use super::swarm_kit::{AgentDef, Endpoint, StartedBroker, SwarmKit};

const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";
const ANALYST: &str = "kit-analyst";
const ANALYST_MODEL: &str = "kit/analyst";
const ROOT_MODEL: &str = "kit/root";

/// The root conversation, on the kit's SSE endpoint, delegating through the
/// kit's broker.
async fn root_session(kit: &SwarmKit) -> AgentSession {
    root_session_with(kit, &[LEAD, ANALYST]).await
}

/// [`root_session`] whose roster is `local_agents`.
async fn root_session_with(kit: &SwarmKit, local_agents: &[&str]) -> AgentSession {
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
                lazy_broker: Some(Arc::new(StartedBroker(kit.broker().transport()))),
                local_agents: local_agents.iter().map(|name| name.to_string()).collect(),
                ..AgentServices::default()
            }),
        )
        .await
        .expect("the root conversation builds");
    session
}

#[tokio::test]
async fn slash_agent_delegation_builds_tree() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(ANALYST, ANALYST_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call(
                    "invoke_agent",
                    json!({ "agent": ANALYST, "prompt": "read the readme" }),
                ),
                Reply::text("The analyst read it: Chatty."),
            ],
        ),
        Script::new().route(
            ANALYST_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                Reply::text("It says Chatty."),
            ],
        ),
    )
    .await;
    let mut session = root_session(&kit).await;

    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(
            TurnInput::delegation(Delegation {
                agent: LEAD.to_string(),
                prompt: "have the analyst read the readme".to_string(),
            }),
            move |event| sink.borrow_mut().push(event),
        )
        .expect("the /agent turn starts");
    tokio::time::timeout(std::time::Duration::from_secs(120), turn)
        .await
        .expect("the /agent turn ends before the deadline");
    let events = Rc::try_unwrap(events).unwrap().into_inner();

    assert!(
        kit.sse.requests_for(ROOT_MODEL).is_empty(),
        "/agent asks the root's model nothing"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::SwarmEvent(_))),
        "the lead's own delegation reaches the root as swarm events: {events:#?}"
    );
    // The same test the desktop's stream manager applies before it hands
    // the transcript a tree: some run sits below the root's callee.
    let mut trace = SwarmTrace::new();
    for event in &events {
        trace.apply(event);
    }
    let tree = trace.tree();
    assert!(
        tree.preorder().into_iter().any(|id| tree.depth(id) >= 2),
        "the analyst sits under the lead in the tree"
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("The analyst read it: Chatty."),
        "the lead's answer is the turn's text: {text}"
    );
}

/// AGE-752: a command the delegated agent asks to run reaches this
/// conversation's approval card while the `/agent` turn is still running —
/// the delegation stream forwards the store's notifications as a model's
/// stream does — and the approval lets the worker run it. Before the fix
/// the notification was dropped and the worker waited out its timeout.
#[tokio::test]
async fn slash_agent_relays_a_worker_approval() {
    use chatty_core::models::execution_approval_store::ApprovalDecision;

    let kit = SwarmKit::start_asking(
        vec![AgentDef::new(ANALYST, ANALYST_MODEL, Endpoint::Ndjson)],
        Script::new(),
        Script::new().route(
            ANALYST_MODEL,
            [
                Reply::tool_call("shell_execute", json!({ "command": "echo hi" })),
                Reply::text("It printed hi."),
            ],
        ),
    )
    .await;
    let mut session = root_session_with(&kit, &[ANALYST]).await;
    let store = session.execution_approvals().clone();

    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(
            TurnInput::delegation(Delegation {
                agent: ANALYST.to_string(),
                prompt: "run echo hi".to_string(),
            }),
            move |event| {
                // The human at the card: approve what the worker asks.
                if let SessionEvent::ApprovalRequested { id, .. } = &event {
                    assert!(store.resolve(id, ApprovalDecision::Approved));
                }
                sink.borrow_mut().push(event);
            },
        )
        .expect("the /agent turn starts");
    tokio::time::timeout(std::time::Duration::from_secs(120), turn)
        .await
        .expect("the /agent turn ends before the deadline");
    let events = Rc::try_unwrap(events).unwrap().into_inner();

    let asked: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ApprovalRequested { command, .. } => Some(command),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked.len(),
        1,
        "one card, for the worker's command: {events:#?}"
    );
    assert!(asked[0].ends_with("[shell] echo hi"), "{asked:?}");
    let analyst = kit.ndjson.requests_for(ANALYST_MODEL);
    assert_eq!(analyst.len(), 2, "the worker's model is asked again");
    let result = analyst[1].json()["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .map(|message| message.to_string())
        .unwrap_or_default();
    assert!(
        result.contains(r#"\"stdout\":\"hi\""#),
        "the approved command ran in the worker: {result}"
    );
}

// ── AGE-747: chatty-tui's own `/agent` dispatch ─────────────────────────────
//
// `slash_agent_delegation_builds_tree` above pins the shared plumbing
// (`AgentSession` + `Delegation` + a broker): that already worked once
// AGE-744 landed. What AGE-747 changes is chatty-tui's own `/agent <name>
// <prompt>` handler (`engine::commands::launch_sub_agent`), which used to
// start a headless `chatty-tui --agent` subprocess with no `--broker` at
// all — so a roster lead launched this way could never itself delegate.
// This test drives the real `ChatEngine`, exactly as `app.rs` does for a
// `KeyAction::LaunchAgent`, to pin that the command now runs as a turn of
// the conversation through its own broker instead.

use chatty_core::agent_spec::AgentSpec;
use chatty_core::settings::models::ModelsModel;
use chatty_core::settings::models::module_settings::ModuleSettingsModel;

use crate::engine::{ChatEngine, ChatEngineConfig};
use crate::events::AppEvent;

/// `/agent lead …` with a lead that delegates builds a two-level tree on
/// the swarm kit — the same tree a model-issued `invoke_agent` would have
/// built, and never a subprocess.
#[tokio::test]
async fn tui_slash_agent_delegates_through_broker() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(ANALYST, ANALYST_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call(
                    "invoke_agent",
                    json!({ "agent": ANALYST, "prompt": "read the readme" }),
                ),
                Reply::text("The analyst read it: Chatty."),
            ],
        ),
        Script::new().route(
            ANALYST_MODEL,
            [
                Reply::tool_call("read_file", json!({ "path": "README.md" })),
                Reply::text("It says Chatty."),
            ],
        ),
    )
    .await;

    let _ = chatty_core::init_repositories();
    let execution_settings = ExecutionSettingsModel {
        workspace_dir: Some(kit.workspace().to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let provider_config = ProviderConfig::new("Fake SSE".to_string(), ProviderType::OpenRouter)
        .with_api_key("swarm-kit-key".to_string())
        .with_base_url(kit.sse.base_url());
    let model_config = ModelConfig::new(
        ROOT_MODEL.to_string(),
        ROOT_MODEL.to_string(),
        ProviderType::OpenRouter,
        ROOT_MODEL.to_string(),
    );
    // The root's own spec, built the way `main.rs` builds the default one
    // for an interactive run with no `--agent`: it delegates.
    let mut root_spec = AgentSpec::named("kit-root");
    root_spec.swarm.delegates_to = vec!["*".to_string()];

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = ChatEngine::new(
        ChatEngineConfig {
            model_config,
            provider_config,
            execution_settings,
            module_settings: ModuleSettingsModel {
                virtual_agents: vec![LEAD.to_string(), ANALYST.to_string()],
                ..ModuleSettingsModel::default()
            },
            broker: Some(Arc::new(StartedBroker(kit.broker().transport()))),
            models: ModelsModel::default(),
            providers: Vec::new(),
            mcp_service: None,
            memory_service: None,
            search_settings: None,
            embedding_service: None,
            user_secrets: Vec::new(),
            remote_agents: Vec::new(),
            spec: root_spec,
            team: None,
            is_sub_agent: false,
            services_loaded: true,
            surface: StreamSurface::InteractiveTui,
        },
        event_tx,
    );
    engine
        .init_conversation()
        .await
        .expect("the root conversation builds");
    // `resolve_agent_command` matches `/agent <name>` against the loaded
    // roster; `kit-lead` names a `SwarmKit` worker, not a spec file on
    // disk, so it is set directly here exactly as a real roster entry
    // would have resolved.
    engine.agent_roster = vec![AgentSpec::named(LEAD)];

    engine
        .launch_sub_agent(&format!("{LEAD} have the analyst read the readme"))
        .expect("/agent dispatches");
    assert!(
        engine.is_streaming,
        "the delegation is a turn of this conversation, not a detached subprocess"
    );

    let mut text = String::new();
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(120), event_rx.recv())
            .await
            .expect("an event arrives before the deadline")
            .expect("the event channel stays open while the turn runs");
        let done = matches!(event, AppEvent::StreamCompleted | AppEvent::StreamError(_));
        if let AppEvent::TextChunk(chunk) = &event {
            text.push_str(chunk);
        }
        engine.handle_event(event);
        if done {
            break;
        }
    }

    assert!(
        kit.sse.requests_for(ROOT_MODEL).is_empty(),
        "/agent asks the root's own model nothing"
    );
    assert!(!engine.is_streaming, "the turn finished");

    // A two-level tree: the lead's own delegation to the analyst reached
    // the root as swarm events, folded live into `engine.swarm_trace` by
    // `handle_event` exactly as `/swarm` relies on.
    let tree = engine.swarm_trace.tree();
    assert!(
        tree.preorder().into_iter().any(|id| tree.depth(id) >= 2),
        "the analyst sits under the lead in the tree"
    );

    assert!(
        text.contains("The analyst read it: Chatty."),
        "the lead's answer reached the transcript: {text}"
    );
}
