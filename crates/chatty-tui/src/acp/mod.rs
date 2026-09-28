//! `chatty-tui acp`: chatty as an Agent Client Protocol agent on stdio
//! (ADR-0013, AGE-414).
//!
//! An ACP client — an editor such as Zed, or Harbor's ACP runner — spawns
//! this process and speaks JSON-RPC over its stdin/stdout. Each
//! `session/new` gets its own [`AgentSession`] rooted at the client's `cwd`;
//! `session/prompt` runs a turn on it and streams the turn's
//! [`SessionEvent`]s back as `session/update` notifications ([`mapping`]);
//! an approval the turn raises becomes `session/request_permission`; and
//! `session/cancel` is the session's cancel switch.
//!
//! No benchmark policy lives here (ADR-0013): the prompt runs exactly as
//! the model and `AgentSession` run it. `ask_user` is off, since
//! `ClarificationRequested` has no ACP representation. Stdout belongs to
//! the protocol, so logging goes to stderr, which clients keep as the
//! agent's log.

mod mapping;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, ContentBlock, EmbeddedResourceResource, Implementation,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PermissionOption,
    PermissionOptionKind, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SessionId, SessionNotification, StopReason,
    ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder, Stdio};
use anyhow::{Context, anyhow};
use chatty_core::factories::agent_factory::{AgentBuildContext, AgentServices};
use chatty_core::services::StreamSurface;
use chatty_core::session::{
    AgentSession, AgentSessionConfig, SessionEvent, TurnInput, turn_transport,
};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use crate::engine::ChatEngineConfig;
use mapping::TurnMapper;

/// The permission option that approves a request; anything else denies it.
const ALLOW_OPTION: &str = "allow";
const REJECT_OPTION: &str = "reject";

/// One ACP session: its `AgentSession`, and the workspace it is rooted at.
///
/// The lock is only held for synchronous work — `begin_turn`, `apply`,
/// `finish_turn`, cancel, resolving an approval — never across the turn,
/// so `session/cancel` reaches a session whose turn is streaming.
#[derive(Clone)]
struct AcpSession {
    session: Arc<Mutex<AgentSession>>,
    workspace: PathBuf,
    /// Tests only: each turn plays the next of these instead of calling
    /// the provider.
    #[cfg(test)]
    scripted: Arc<std::sync::Mutex<std::collections::VecDeque<chatty_core::services::Scenario>>>,
}

/// Everything a new session is built from, and the live sessions.
struct Server {
    config: ChatEngineConfig,
    sessions: std::sync::Mutex<HashMap<String, AcpSession>>,
}

/// Serve ACP on stdio until the client closes stdin.
pub async fn run(config: ChatEngineConfig) -> anyhow::Result<()> {
    let server = Arc::new(Server {
        config,
        sessions: Default::default(),
    });
    use agent_client_protocol::ConnectTo as _;
    agent(server)
        .connect_to(Stdio::new())
        .await
        .map_err(|e| anyhow!("ACP connection failed: {e}"))
}

/// The agent: its handlers, ready to connect to a client — `Stdio` in
/// production, an in-memory client in tests.
fn agent(server: Arc<Server>) -> impl agent_client_protocol::ConnectTo<Client> {
    let new_session_server = server.clone();
    let prompt_server = server.clone();
    let cancel_server = server;
    Agent
        .builder()
        .name("chatty")
        .on_receive_request(
            async move |request: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx: ConnectionTo<Client>| {
                responder.respond(initialize_response(&request))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        cx: ConnectionTo<Client>| {
                // Building the agent takes a while (MCP tools, the
                // provider client); do it off the dispatch loop.
                let server = new_session_server.clone();
                cx.spawn(async move {
                    match server.new_session(request).await {
                        Ok(response) => responder.respond(response),
                        Err(e) => {
                            warn!(error = %format!("{e:#}"), "session/new failed");
                            responder.respond_with_error(internal_error(format!("{e:#}")))
                        }
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest,
                        responder: Responder<PromptResponse>,
                        cx: ConnectionTo<Client>| {
                let Some(entry) = prompt_server.get(&request.session_id) else {
                    return responder.respond_with_error(
                        agent_client_protocol::Error::invalid_params().data(serde_json::json!(
                            format!("unknown session {}", request.session_id.0)
                        )),
                    );
                };
                let prompt = prompt_text(&request.prompt);
                // The turn runs for minutes and waits on the client for
                // permissions; the dispatch loop must stay free for those
                // responses and for `session/cancel`.
                let connection = cx.clone();
                cx.spawn(async move {
                    match run_prompt(&connection, &request.session_id, &entry, prompt).await {
                        Ok(stop_reason) => responder.respond(PromptResponse::new(stop_reason)),
                        Err(message) => responder.respond_with_error(internal_error(message)),
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, _cx: ConnectionTo<Client>| {
                if let Some(entry) = cancel_server.get(&notification.session_id) {
                    let session = entry.session.lock().await;
                    session.cancel();
                    session.clarifications().cancel_all();
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
}

fn initialize_response(request: &InitializeRequest) -> InitializeResponse {
    // v1 is the only version this agent speaks; a client asking for a
    // later one is told so and decides whether it can go on.
    let version = if request.protocol_version > ProtocolVersion::V1 {
        ProtocolVersion::V1
    } else {
        request.protocol_version
    };
    InitializeResponse::new(version)
        .agent_capabilities(
            AgentCapabilities::new()
                .prompt_capabilities(PromptCapabilities::new().embedded_context(true)),
        )
        .agent_info(Implementation::new("chatty", crate::APP_VERSION).title("Chatty"))
}

fn internal_error(message: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(serde_json::json!(message.into()))
}

impl Server {
    fn get(&self, id: &SessionId) -> Option<AcpSession> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id.0.as_ref())
            .cloned()
    }

    /// Build a session rooted at the client's `cwd`, the same way a
    /// headless run builds its one conversation.
    async fn new_session(&self, request: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
        let workspace = std::fs::canonicalize(&request.cwd)
            .with_context(|| format!("cwd '{}' is not usable", request.cwd.display()))?;
        if !workspace.is_dir() {
            anyhow::bail!("cwd '{}' is not a directory", workspace.display());
        }
        if !request.mcp_servers.is_empty() {
            // chatty connects to its own configured MCP servers; the
            // client's are not bridged yet.
            warn!(
                count = request.mcp_servers.len(),
                "Ignoring MCP servers passed by the ACP client"
            );
        }

        let config = &self.config;
        let mut settings = config.execution_settings.clone();
        settings.workspace_dir = Some(workspace.to_string_lossy().to_string());
        settings.ask_user_enabled = false;

        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings: settings.clone(),
            surface: StreamSurface::InteractiveTui,
            // Nothing outside the session watches the turn for loops, as
            // headless's own loop does; the desktop runs with it on too.
            loop_guard: true,
        });
        let mcp_tools = match config.mcp_service {
            Some(ref svc) => chatty_core::services::gather_mcp_tools(svc).await,
            None => None,
        };
        // `settings` carries `ask_user_enabled: false` and the ACP session's
        // workspace, which `from_spec` reads for `ask_user_enabled` and
        // `instructions_dir`.
        let built = AgentBuildContext::from_spec(
            &config.spec,
            AgentServices {
                exec_settings: Some(settings.clone()),
                user_secrets: config.user_secrets.clone(),
                memory_service: config.memory_service.clone(),
                skill_service: Some(chatty_core::services::SkillService::new(
                    config.embedding_service.clone(),
                )),
                search_settings: config.search_settings.clone(),
                embedding_service: config.embedding_service.clone(),
                gateway_port: config.broker_port.or(config
                    .module_settings
                    .enabled
                    .then_some(config.module_settings.gateway_port)),
                lazy_broker: config.broker.clone(),
                local_agents: config
                    .module_settings
                    .roster_names(settings.workspace_dir.as_deref().map(std::path::Path::new)),
                remote_agents: config.remote_agents.clone(),
                plugin_host: crate::engine::plugin_host(
                    &config.module_settings,
                    &config.models,
                    &config.providers,
                ),
            },
        )
        .map_err(anyhow::Error::from)?;
        let ctx = AgentBuildContext {
            mcp_tools,
            ..built.context
        };
        let id = uuid::Uuid::new_v4().to_string();
        session
            .create_conversation(
                id.clone(),
                "ACP session".to_string(),
                &config.model_config,
                &config.provider_config,
                ctx,
            )
            .await
            .context("Failed to create conversation")?;
        info!(session = %id, workspace = %workspace.display(), "ACP session started");
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                AcpSession {
                    session: Arc::new(Mutex::new(session)),
                    workspace,
                    #[cfg(test)]
                    scripted: Default::default(),
                },
            );
        Ok(NewSessionResponse::new(id))
    }
}

/// The prompt as the one user message the model gets: text as is, a
/// mentioned file by its URI, an embedded file with its contents.
fn prompt_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => parts.push(text.text.clone()),
            ContentBlock::ResourceLink(link) => parts.push(format!("@{}", link.uri)),
            ContentBlock::Resource(resource) => match &resource.resource {
                EmbeddedResourceResource::TextResourceContents(contents) => parts.push(format!(
                    "<context uri=\"{}\">\n{}\n</context>",
                    contents.uri, contents.text
                )),
                EmbeddedResourceResource::BlobResourceContents(contents) => {
                    parts.push(format!("@{}", contents.uri))
                }
                _ => {}
            },
            // Not advertised in `prompt_capabilities`.
            _ => {}
        }
    }
    parts.join("\n")
}

/// Run one prompt to its end and say why it ended.
///
/// A prompt can be more than one turn: a `FollowUp` the session asks for
/// after a turn (the agent protocol's own continuation) runs inside the
/// same prompt, since the client only sends the next prompt once this one
/// has answered. A stream error is the prompt's error.
async fn run_prompt(
    cx: &ConnectionTo<Client>,
    session_id: &SessionId,
    entry: &AcpSession,
    prompt: String,
) -> Result<StopReason, String> {
    let mut next = Some(TurnInput::text(prompt));
    let mut stop_reason = StopReason::EndTurn;
    while let Some(input) = next.take() {
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();
        start_turn(entry, input, move |event| {
            let _ = tx.send(event);
        })
        .await?;

        let mut mapper = TurnMapper::new(entry.workspace.clone());
        let mut error = None;
        // To the end of the channel, not to `TurnEnded`: `FollowUp` comes
        // after it.
        while let Some(event) = rx.recv().await {
            let plan = {
                let mut session = entry.session.lock().await;
                let plan = session.apply(&event);
                if matches!(event, SessionEvent::TurnEnded) {
                    session.finish_turn(None, Vec::new());
                }
                plan
            };
            let mut updates = mapper.map(&event);
            if let Some(snapshot) = plan {
                updates.push(mapping::plan_update(&snapshot));
            }
            for update in updates {
                if let Err(e) =
                    cx.send_notification(SessionNotification::new(session_id.clone(), update))
                {
                    warn!(error = %e, "Failed to send session/update");
                }
            }
            match event {
                SessionEvent::ApprovalRequested { id, command, .. } => {
                    let tool_call = mapper.running_tool_call().unwrap_or(&id).to_string();
                    let approved = ask_permission(cx, session_id, tool_call, command).await;
                    let resolve = {
                        let session = entry.session.lock().await;
                        turn_transport::resolve_approval(&session, None, &id, approved)
                    };
                    resolve.await;
                }
                // Not advertised and `ask_user` is off; should one still
                // arrive, unblock the tool rather than let it time out.
                SessionEvent::ClarificationRequested { .. } => {
                    entry.session.lock().await.clarifications().cancel_all();
                }
                SessionEvent::Error(e) => error = Some(e.to_string()),
                SessionEvent::Cancelled => stop_reason = StopReason::Cancelled,
                SessionEvent::FollowUp(prompt)
                    if error.is_none() && stop_reason != StopReason::Cancelled =>
                {
                    next = Some(TurnInput::protocol_follow_up(prompt));
                }
                _ => {}
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
    }
    Ok(stop_reason)
}

/// Start a turn on the session and leave it running; its events go to
/// `emit`.
async fn start_turn(
    entry: &AcpSession,
    input: TurnInput,
    emit: impl FnMut(SessionEvent) + Send + 'static,
) -> Result<(), String> {
    let mut session = entry.session.lock().await;
    #[cfg(test)]
    if let Some(scenario) = entry.scripted.lock().unwrap().pop_front() {
        let turn = session
            .begin_scripted_turn(input, scenario, emit)
            .map_err(|e| format!("{e:#}"))?;
        tokio::spawn(turn);
        return Ok(());
    }
    let turn = session
        .begin_turn(input, emit)
        .map_err(|e| format!("{e:#}"))?;
    tokio::spawn(turn);
    Ok(())
}

/// Ask the client whether the running tool may go ahead. A cancelled
/// prompt, a closed connection or any other answer than "allow" denies.
async fn ask_permission(
    cx: &ConnectionTo<Client>,
    session_id: &SessionId,
    tool_call_id: String,
    command: String,
) -> bool {
    let request = RequestPermissionRequest::new(
        session_id.clone(),
        ToolCallUpdate::new(tool_call_id, ToolCallUpdateFields::new().title(command)),
        vec![
            PermissionOption::new(ALLOW_OPTION, "Allow", PermissionOptionKind::AllowOnce),
            PermissionOption::new(REJECT_OPTION, "Deny", PermissionOptionKind::RejectOnce),
        ],
    );
    match cx.send_request(request).block_task().await {
        Ok(response) => matches!(
            response.outcome,
            RequestPermissionOutcome::Selected(ref selected)
                if selected.option_id.0.as_ref() == ALLOW_OPTION
        ),
        Err(e) => {
            warn!(error = %e, "session/request_permission failed; denying");
            false
        }
    }
}

#[cfg(test)]
mod tests;
