//! `chatty-tui acp` end to end: an in-memory ACP client drives the agent
//! through `initialize` → `session/new` → `session/prompt`, with each turn
//! played from a scripted stream instead of a provider.

use std::sync::Mutex as StdMutex;
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ContentBlock, EmbeddedResource, RequestPermissionResponse, ResourceLink,
    SelectedPermissionOutcome, TextContent, TextResourceContents,
};
use chatty_core::services::{Scenario, ScriptedItem, StreamChunk, scenarios};
use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
use chatty_core::settings::models::models_store::{ModelConfig, ModelsModel};
use chatty_core::settings::models::module_settings::ModuleSettingsModel;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};

use super::*;

const TIMEOUT: Duration = Duration::from_secs(30);

/// A server around a network-free Ollama model, as the headless tests use.
fn test_server() -> Arc<Server> {
    let _ = chatty_core::init_repositories();
    Arc::new(Server {
        config: ChatEngineConfig {
            model_config: ModelConfig::new(
                "m1".to_string(),
                "Test Model".to_string(),
                ProviderType::Ollama,
                "llama3.2".to_string(),
            ),
            provider_config: ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
            execution_settings: ExecutionSettingsModel::default(),
            module_settings: ModuleSettingsModel::default(),
            broker_port: None,
            models: ModelsModel::default(),
            providers: Vec::new(),
            mcp_service: None,
            memory_service: None,
            search_settings: None,
            embedding_service: None,
            user_secrets: Vec::new(),
            remote_agents: Vec::new(),
            module_agents: Vec::new(),
            role: Default::default(),
            team: None,
            is_sub_agent: false,
            services_loaded: true,
            surface: StreamSurface::InteractiveTui,
        },
        sessions: Default::default(),
    })
}

fn scenario(name: &str) -> Scenario {
    scenarios()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no scenario {name}"))
}

/// What the client saw: every `session/update`, as JSON, and every
/// permission request's tool call id.
#[derive(Default)]
struct Seen {
    updates: StdMutex<Vec<serde_json::Value>>,
    permissions: StdMutex<Vec<String>>,
}

/// Run `initialize`, `session/new` in a scratch workspace, then `prompt`
/// with `turns` scripted, and return the stop reason and what was seen.
async fn prompt_once(
    turns: Vec<Scenario>,
    prompt: Vec<ContentBlock>,
    allow: bool,
) -> (agent_client_protocol::Result<StopReason>, Arc<Seen>) {
    let server = test_server();
    let workspace = tempfile::tempdir().unwrap();
    let seen = Arc::new(Seen::default());
    let turns = Arc::new(StdMutex::new(Some(turns)));

    let on_update = seen.clone();
    let on_permission = seen.clone();
    let agent_server = server.clone();
    let cwd = workspace.path().to_path_buf();
    let result = Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx: ConnectionTo<Agent>| {
                on_update
                    .updates
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(&notification.update).unwrap());
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest,
                        responder: Responder<RequestPermissionResponse>,
                        _cx: ConnectionTo<Agent>| {
                on_permission
                    .permissions
                    .lock()
                    .unwrap()
                    .push(request.tool_call.tool_call_id.0.to_string());
                let option = if allow { ALLOW_OPTION } else { REJECT_OPTION };
                responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option)),
                ))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent(agent_server), async move |cx| {
            let init = cx
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            assert_eq!(init.protocol_version, ProtocolVersion::V1);
            let session = cx
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            let entry = server.get(&session.session_id).expect("session registered");
            *entry.scripted.lock().unwrap() = turns.lock().unwrap().take().unwrap().into();
            let response = cx
                .send_request(PromptRequest::new(session.session_id, prompt))
                .block_task()
                .await?;
            Ok(response.stop_reason)
        });
    let result = tokio::time::timeout(TIMEOUT, result)
        .await
        .expect("the prompt did not finish");
    (result, seen)
}

fn text(s: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text(TextContent::new(s))]
}

fn kinds(seen: &Seen) -> Vec<String> {
    seen.updates
        .lock()
        .unwrap()
        .iter()
        .map(|u| u["sessionUpdate"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_text_answer_streams_as_message_chunks_and_ends_the_turn() {
    let (result, seen) = prompt_once(vec![scenario("text_only")], text("hi"), true).await;
    assert_eq!(result.unwrap(), StopReason::EndTurn);
    let updates = seen.updates.lock().unwrap();
    let streamed: String = updates
        .iter()
        .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
        .map(|u| u["content"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(streamed, "Hello, world");
}

#[tokio::test]
async fn a_tool_round_trip_is_a_tool_call_and_its_updates() {
    let (result, seen) = prompt_once(
        vec![scenario("tool_call_then_result")],
        text("read it"),
        true,
    )
    .await;
    assert_eq!(result.unwrap(), StopReason::EndTurn);
    assert_eq!(
        kinds(&seen),
        [
            "tool_call",
            "tool_call_update",
            "tool_call_update",
            "agent_message_chunk"
        ]
    );
    let updates = seen.updates.lock().unwrap();
    assert_eq!(updates[0]["toolCallId"], "call-1");
    assert_eq!(updates[2]["status"], "completed");
    assert_eq!(updates[2]["title"], "Read README.md");
}

#[tokio::test]
async fn an_approval_asks_the_client_for_the_running_tool_call() {
    let (result, seen) = prompt_once(vec![scenario("approval_granted")], text("clean"), true).await;
    assert_eq!(result.unwrap(), StopReason::EndTurn);
    assert_eq!(*seen.permissions.lock().unwrap(), ["call-1"]);
}

#[tokio::test]
async fn a_cancelled_turn_ends_the_prompt_as_cancelled() {
    let cancelled = Scenario {
        name: "cancel",
        progress: Vec::new(),
        items: vec![
            ScriptedItem::CancelThen(StreamChunk::Text("partial".into())),
            ScriptedItem::Chunk(StreamChunk::Done),
        ],
    };
    let (result, _) = prompt_once(vec![cancelled], text("go"), true).await;
    assert_eq!(result.unwrap(), StopReason::Cancelled);
}

#[tokio::test]
async fn a_stream_error_is_the_prompts_error() {
    let failing = Scenario {
        name: "fail",
        progress: Vec::new(),
        items: vec![ScriptedItem::Failure("connection reset".into())],
    };
    let (result, _) = prompt_once(vec![failing], text("go"), true).await;
    let error = result.expect_err("the prompt should fail");
    assert!(
        format!("{error:?}").contains("connection reset"),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_prompt_for_an_unknown_session_is_refused() {
    let result = Client
        .builder()
        .connect_with(agent(test_server()), async move |cx| {
            cx.send_request(PromptRequest::new("nope", text("hi")))
                .block_task()
                .await
                .map(|_| ())
        });
    let result = tokio::time::timeout(TIMEOUT, result).await.unwrap();
    assert!(result.is_err());
}

#[test]
fn prompt_text_inlines_mentions_and_embedded_files() {
    let prompt = prompt_text(&[
        ContentBlock::Text(TextContent::new("Explain")),
        ContentBlock::ResourceLink(ResourceLink::new("main.rs", "file:///w/src/main.rs")),
        ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                "fn main() {}",
                "file:///w/src/lib.rs",
            )),
        )),
    ]);
    assert_eq!(
        prompt,
        "Explain\n@file:///w/src/main.rs\n<context uri=\"file:///w/src/lib.rs\">\nfn main() {}\n</context>"
    );
}
