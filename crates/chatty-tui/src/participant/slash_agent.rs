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
                local_agents: vec![LEAD.to_string(), ANALYST.to_string()],
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
