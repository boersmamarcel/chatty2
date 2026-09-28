use parking_lot::Mutex;
use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::models::clarification_store::{PendingClarifications, request_clarification};
use crate::models::message_types::ToolSource;
use crate::models::token_usage::TokenUsage;
use crate::services::a2a_client::{
    A2aClarificationRequest, A2aClient, A2aStreamEvent, conversation_from_status_metadata,
    trace_from_status_metadata, usage_from_status_metadata,
};
use crate::services::fabric_transport::progress_from_value;
use crate::services::handoff::{HandoffLedger, HandoffReport};
use crate::services::lazy_broker::LazyBroker;
use crate::services::spend_gate::{CapExceeded, SpendGate};
use crate::settings::models::a2a_store::A2aAgentConfig;
use chatty_fabric::{
    AgentOrigin, CallError, CallEvent, CallRequest, InvokeAgentOutcome, InvokeAgentParams, Refusal,
    Transport,
};

/// The agent name the broker publishes for "a chatty agent in its own
/// process" (ADR-0011 C2).
///
/// Defined here rather than in the gateway because both ends need it and the
/// gateway cannot depend on this crate; the wiring that starts the broker
/// passes this constant to `LocalRunner::with_agent_name`, so the two cannot
/// drift.
pub const LOCAL_AGENT_NAME: &str = "local-agent";

/// Progress events emitted by the invoke_agent tool during streaming execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum InvokeAgentProgress {
    /// Agent invocation started.
    Started {
        agent_name: String,
        prompt: String,
        source: ToolSource,
    },
    /// A text chunk from the agent's response.
    Text(String),
    /// A line the agent reported about its own work — a tool call starting
    /// or finishing, as `progress_text_for_event` renders it — rather than
    /// part of its answer. Kept apart from [`Text`](Self::Text) so a worker
    /// that delegated can pass its callee's steps one level further up
    /// without passing up the callee's answer as well (BI-4, AGE-636).
    Step(String),
    /// Agent invocation finished.
    Finished {
        success: bool,
        result: Option<String>,
        /// What the agent spent, as reported on its terminal status: one
        /// line per model, each naming its model, with `delegated_to`
        /// naming the agent (AGE-415, AGE-682). The session folds them into
        /// the conversation's usage as delegated lines, so the leader's
        /// totals include what its workers spent. Empty when the agent
        /// reported nothing — a remote agent that does not say, or a task
        /// that never reached its terminal status.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        usage: Vec<TokenUsage>,
    },
}

/// Shared slot for sending progress events from the tool to the stream loop.
///
/// The stream loop installs a fresh sender before each LLM stream. The tool
/// holds a reference to the slot and sends progress events through it.
pub type InvokeAgentProgressSlot = Arc<Mutex<Option<UnboundedSender<InvokeAgentProgress>>>>;

/// Arguments for the invoke_agent tool
#[derive(Deserialize, Serialize)]
pub struct InvokeAgentArgs {
    /// Name of the agent to invoke (must match a known remote or local agent).
    pub agent: String,
    /// The prompt or task to send to the agent.
    pub prompt: String,
    /// Ask the worker for its compacted tool-call trace alongside its
    /// response (AGE-467). Off by default: the parent's context must not
    /// grow unless it asks for this.
    #[serde(default)]
    pub include_trace: bool,
}

/// Output from the invoke_agent tool
#[derive(Debug, Serialize)]
pub struct InvokeAgentOutput {
    /// The agent's response text.
    pub response: String,
    /// Name of the agent that was invoked.
    pub agent: String,
    /// Whether the invocation completed successfully.
    pub success: bool,
    /// The worker's compacted tool-call trace (AGE-467), present only when
    /// `include_trace` was set and the worker's terminal status carried one.
    /// Absent from the JSON the model sees otherwise, so a plain delegation
    /// costs no more context than it did before this field existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    /// The worker's captured conversation (RC-0, AGE-649), when its terminal
    /// status carried one. Test-support only for now: nothing sets the
    /// broker-side capture flag outside a test, and nothing here reads this
    /// field back into a session; RC-3 is what a leader does with it.
    /// Absent from the JSON the model sees otherwise, for the same reason
    /// `trace` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<serde_json::Value>,
    /// The worker's typed handoff (TD-2, AGE-693): the JSON its final
    /// answer carried, already checked against its role's schema. Present
    /// only when the team names a schema for that role; absent from the JSON
    /// the model sees otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<serde_json::Value>,
}

/// Error type for invoke_agent tool
#[derive(Debug, thiserror::Error)]
pub enum InvokeAgentError {
    #[error("Agent not found: {0}")]
    NotFound(String),
    #[error("Agent disabled: {0}")]
    Disabled(String),
    #[error("Invocation failed: {0}")]
    InvocationFailed(String),
    /// The tenant's monthly cap is spent, so no delegation was started
    /// (AGE-416). The text is the [`CapExceeded`] display —
    /// `cap_exceeded: month-to-date $X.XX >= cap $Y.YY; no delegation
    /// started` — and the payload carries the same three fields hive's
    /// `402` body does.
    #[error(transparent)]
    CapExceeded(CapExceeded),
    /// The worker's handoff failed its role's schema twice: once, and again
    /// after the one follow-up turn that listed the errors (TD-2, AGE-693).
    /// The model sees `Error: invoke_agent: handoff_invalid: <role>: …`.
    #[error("handoff_invalid: {role}: {}", errors.join("; "))]
    HandoffInvalid { role: String, errors: Vec<String> },
    /// The broker refused the delegation before anything was spawned: the
    /// specs do not allow it, it would close a cycle, or it is too deep
    /// (PL-S2). The text is the [`Refusal`] display — `cycle: root → a → a`,
    /// `too_deep: depth 5 > max 4`, `not_listed: a may not call b`.
    #[error(transparent)]
    Refused(Refusal),
}

/// Tool that invokes a named agent with a prompt: a remote A2A agent, or one of
/// the agent specs the broker serves (PL-U5 — one kind of local agent; a WASM
/// plugin is a tool inside one, never an agent).
///
/// Remote agents are called via `A2aClient::send_message_stream()` for real-time progress.
/// Local specs are reached through the broker, over the fabric or its A2A endpoint.
#[derive(Clone)]
pub struct InvokeAgentTool {
    /// Snapshot of configured remote A2A agents taken at construction time.
    remote_agents: Vec<A2aAgentConfig>,
    /// Base URL for the protocol gateway (e.g. `http://localhost:8420`),
    /// used to call the broker's local agents via their A2A endpoint.
    gateway_base_url: Option<String>,
    client: A2aClient,
    /// Shared slot for sending progress events to the UI stream loop.
    progress_slot: InvokeAgentProgressSlot,
    /// Names of the broker's local-worker agents, when any are published
    /// (ADR-0011 C2): the roster's specs (C10, PL-U5). Resolved after remote
    /// agents.
    local_agents: Vec<String>,
    /// Whether to say something before handing a prompt to an agent outside
    /// this user's fleet (ADR-0011 C5).
    warn_outside_fleet: bool,
    /// This agent's own clarification store: where a delegated agent's
    /// question is re-asked (AGE-306). `None` means nobody here can answer,
    /// and a question ends the delegation.
    ///
    /// Escalating to a human is the only policy (ADR-0011 C7). Whether a
    /// leader may instead answer for its worker is an open question on the
    /// ADR; nothing selects such a policy today.
    clarifications: Option<PendingClarifications>,
    /// The hosted per-user spend cap (AGE-416 / ADR-0010), asked before any
    /// delegation starts. `None` — the desktop, chatty-tui, any leader
    /// without a cap — means no check at all.
    spend_gate: Option<Arc<dyn SpendGate>>,
    /// A broker that has not necessarily started yet (BI-2, AGE-634).
    /// Consulted only when `gateway_base_url` is `None`: the production
    /// desktop/chatty-tui wiring hands this in instead of a pre-resolved
    /// port, and the first local delegation actually starts it.
    lazy_broker: Option<Arc<dyn LazyBroker>>,
    /// The fabric this agent reaches its local roles through (ADR-0020,
    /// BI-4): a worker's broker-made connection, or the root's direct
    /// handle into its own broker. When one is present, a local role is
    /// never reached over loopback HTTP; remote agents still go through
    /// `A2aClient`.
    transport: Option<Arc<dyn Transport>>,
    /// Where this leader records its roles' handoffs (TD-2, AGE-693): the
    /// invalid count per role and read-rule misreads. Only a `--team`
    /// leader whose team names `handoffs` has one.
    handoff_ledger: Option<HandoffLedger>,
}

impl InvokeAgentTool {
    pub fn new(remote_agents: Vec<A2aAgentConfig>, gateway_port: Option<u16>) -> Self {
        let gateway_base_url = gateway_port.map(|port| format!("http://localhost:{}", port));
        Self {
            remote_agents,
            gateway_base_url,
            client: A2aClient::for_delegation(),
            progress_slot: Arc::new(Mutex::new(None)),
            local_agents: Vec::new(),
            warn_outside_fleet: false,
            clarifications: None,
            spend_gate: None,
            lazy_broker: None,
            transport: None,
            handoff_ledger: None,
        }
    }

    /// Record every delegation's handoff in `ledger` (TD-2, AGE-693).
    pub fn with_handoff_ledger(mut self, ledger: HandoffLedger) -> Self {
        self.handoff_ledger = Some(ledger);
        self
    }

    /// Reach local roles through `transport` instead of the gateway's
    /// loopback HTTP (ADR-0020, BI-4): a worker's broker-made connection.
    pub fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// The fabric local roles are reached through: the one this tool was
    /// given, else the lazy broker's direct handle (starting it), else
    /// none, and local roles go over the gateway's URL.
    async fn fabric_transport(&self) -> Option<Arc<dyn Transport>> {
        if let Some(transport) = &self.transport {
            return Some(transport.clone());
        }
        let broker = self.lazy_broker.as_ref()?;
        match broker.transport().await {
            Ok(transport) => transport,
            Err(error) => {
                warn!(%error, "Failed to start the broker for a delegation");
                None
            }
        }
    }

    /// Start the broker itself on first use instead of expecting it
    /// already running (BI-2, AGE-634). Ignored when a `gateway_port` was
    /// already given to [`Self::new`]: that path is for tests and hosts
    /// that already know a live port.
    pub fn with_lazy_broker(mut self, broker: Arc<dyn LazyBroker>) -> Self {
        self.lazy_broker = Some(broker);
        self
    }

    /// The gateway's base URL, resolving a [`LazyBroker`] on first use if
    /// that is all this tool has. `None` means there is nothing to delegate
    /// through — no port, no lazy broker.
    async fn gateway_base_url(&self) -> Option<String> {
        if let Some(url) = &self.gateway_base_url {
            return Some(url.clone());
        }
        let broker = self.lazy_broker.as_ref()?;
        match broker.ensure_started().await {
            Ok(url) => Some(url),
            Err(error) => {
                warn!(%error, "Failed to start the broker for a delegation");
                None
            }
        }
    }

    /// Re-ask a delegated agent's questions on this agent's own `ask_user`
    /// surface (AGE-306). The same store the agent's `AskUserTool` holds,
    /// so a question from below is indistinguishable, to whoever answers,
    /// from one this agent asked itself.
    pub fn with_clarifications(mut self, pending: PendingClarifications) -> Self {
        self.clarifications = Some(pending);
        self
    }

    /// Offer the broker's local-worker agents (ADR-0011 C2), each of which
    /// spawns a chatty child per task — the roster's specs, `local-agent`
    /// first (C10, PL-U5). Only meaningful when the gateway is running,
    /// since that is what serves them.
    pub fn with_local_agents<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.local_agents = names.into_iter().map(Into::into).collect();
        self
    }

    /// Ask `gate` before every delegation (AGE-416): month-to-date against
    /// the tenant's cap, nothing more. A refusal fails the call with
    /// [`InvokeAgentError::CapExceeded`] before any HTTP call is made, so no
    /// worker is spawned and no participant registers. A delegation that is
    /// already running is never touched.
    pub fn with_spend_gate(mut self, gate: Arc<dyn SpendGate>) -> Self {
        self.spend_gate = Some(gate);
        self
    }

    /// Warn before handing a prompt to an agent outside this user's fleet
    /// (ADR-0011 C5), from `execution_settings.warn_on_external_agent`.
    ///
    /// What the warning *does* is deliberately nothing but say so. Refusing,
    /// or remembering an answer, is a trust policy nobody has chosen yet; this
    /// is where it will attach.
    pub fn with_external_agent_warning(mut self, warn: bool) -> Self {
        self.warn_outside_fleet = warn;
        self
    }

    /// Say that a prompt is about to leave this user's machines.
    ///
    /// Goes to the progress channel rather than the log, because the person
    /// who needs to know is the user watching the turn.
    fn warn_if_outside_the_fleet(&self, agent: &str, origin: AgentOrigin, url: &str) {
        if !self.warn_outside_fleet || origin.is_own_fleet() {
            return;
        }
        warn!(
            agent = %agent,
            %origin,
            url = %url,
            "Handing a prompt to an agent outside this user's fleet"
        );
        self.send_progress(InvokeAgentProgress::Text(format!(
            "\u{26a0} '{agent}' runs at {url}, outside your fleet ({origin}):              the prompt and anything quoted in it leave this machine."
        )));
    }

    /// Returns a clone of the progress slot for the stream loop to install a sender.
    pub fn progress_slot(&self) -> InvokeAgentProgressSlot {
        self.progress_slot.clone()
    }

    /// Send a progress event through the slot (if a sender is installed).
    fn send_progress(&self, event: InvokeAgentProgress) {
        let guard = self.progress_slot.lock();
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(event);
        }
    }
}

impl Tool for InvokeAgentTool {
    const NAME: &'static str = "invoke_agent";
    type Error = InvokeAgentError;
    type Args = InvokeAgentArgs;
    type Output = InvokeAgentOutput;

    fn description(&self) -> String {
        "Invoke a named agent (remote A2A or local WASM module) with a prompt \
                          and return its response. Use `list_agents` first to discover available \
                          agents. Remote A2A agents are called over HTTP. Local module agents are \
                          called via the protocol gateway. The agent runs autonomously and returns \
                          its final response."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "The name of the agent to invoke. Must match a name from \
                                   `list_agents` output (e.g. \"benford-agent\")."
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt or task to send to the agent."
                },
                "include_trace": {
                    "type": "boolean",
                    "description": "Return the worker's compacted tool trace (calls, inputs, \
                                   outputs) alongside its response. Off by default; costs \
                                   context. Use only when you must judge *how* the worker \
                                   reached its answer, e.g. to compare several workers' \
                                   derivations."
                }
            },
            "required": ["agent", "prompt"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let agent_name = args.agent.trim().to_string();
        let prompt = args.prompt.trim().to_string();

        if agent_name.is_empty() {
            return Err(InvokeAgentError::NotFound(
                "Agent name cannot be empty".to_string(),
            ));
        }
        if prompt.is_empty() {
            return Err(InvokeAgentError::InvocationFailed(
                "Prompt cannot be empty".to_string(),
            ));
        }

        // 0. The tenant's cap (AGE-416), ahead of resolving the agent: a
        //    leader over its cap starts nothing, so nothing below — the
        //    `Started` progress event, the `message/stream` call that makes
        //    the broker spawn a worker — happens. Without a gate this is a
        //    no-op and the call is exactly what it was.
        if let Some(gate) = self.spend_gate.as_ref()
            && let Err(refused) = gate.check().await
        {
            warn!(
                agent = %agent_name,
                month_to_date_usd = refused.month_to_date_usd,
                cap_usd = refused.cap_usd,
                "Refusing to delegate: the spend cap is exceeded"
            );
            return Err(InvokeAgentError::CapExceeded(refused));
        }

        // 1. Check remote A2A agents first (they take precedence)
        if let Some(config) = self.remote_agents.iter().find(|a| a.name == agent_name) {
            if !config.enabled {
                return Err(InvokeAgentError::Disabled(format!(
                    "Remote agent '{}' is disabled. Enable it in Settings → A2A Agents.",
                    agent_name
                )));
            }

            info!(agent = %agent_name, url = %config.url, "Invoking remote A2A agent");
            self.warn_if_outside_the_fleet(&agent_name, AgentOrigin::RemoteConfigured, &config.url);
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: agent_name.clone(),
                prompt: prompt.clone(),
                source: ToolSource::ExternalService {
                    name: agent_name.clone(),
                },
            });
            return self
                .call_streaming(config, &prompt, args.include_trace)
                .await;
        }

        // 2. The broker's local agents: a spec, run as a chatty child in its
        //    own process.
        if let Some(local) = self
            .local_agents
            .iter()
            .map(String::as_str)
            .find(|local| *local == agent_name)
        {
            if let Some(transport) = self.fabric_transport().await {
                info!(agent = %local, "Delegating to a local worker over the fabric");
                self.send_progress(InvokeAgentProgress::Started {
                    agent_name: local.to_string(),
                    prompt: prompt.clone(),
                    source: ToolSource::Local,
                });
                return self
                    .call_over_fabric(transport.as_ref(), local, &prompt, args.include_trace)
                    .await;
            }

            let Some(base_url) = self.gateway_base_url().await else {
                return Err(InvokeAgentError::InvocationFailed(format!(
                    "Agent '{local}' needs the protocol gateway. \
                         Enable it in Settings \u{2192} Modules."
                )));
            };

            info!(agent = %local, "Delegating to a local worker through the broker");
            let config = A2aAgentConfig {
                name: local.to_string(),
                url: format!("{}/a2a/{}", base_url, local),
                api_key: None,
                enabled: true,
                skills: vec!["delegate".to_string()],
            };
            self.send_progress(InvokeAgentProgress::Started {
                agent_name: local.to_string(),
                prompt: prompt.clone(),
                source: ToolSource::Local,
            });
            return self
                .call_streaming(&config, &prompt, args.include_trace)
                .await;
        }

        // 3. Not found
        let available: Vec<String> = self
            .remote_agents
            .iter()
            .map(|a| a.name.clone())
            .chain(self.local_agents.iter().cloned())
            .collect();

        Err(InvokeAgentError::NotFound(format!(
            "No agent named '{}'. Available agents: {}",
            agent_name,
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        )))
    }
}

impl InvokeAgentTool {
    /// Call an agent via streaming A2A protocol, forwarding progress events.
    async fn call_streaming(
        &self,
        config: &A2aAgentConfig,
        prompt: &str,
        include_trace: bool,
    ) -> Result<InvokeAgentOutput, InvokeAgentError> {
        use futures::StreamExt;

        let mut stream = self
            .client
            .send_message_stream(config, prompt)
            .await
            .map_err(|e| {
                let err_text = format!("⚠️ Failed to invoke agent '{}': {}", config.name, e);
                self.send_progress(InvokeAgentProgress::Finished {
                    success: false,
                    result: Some(err_text),
                    usage: Vec::new(),
                });
                InvokeAgentError::InvocationFailed(format!(
                    "Failed to invoke agent '{}': {}",
                    config.name, e
                ))
            })?;

        let mut response = String::new();
        let mut success = true;
        let mut error_msg = None;
        let mut usage = Vec::new();
        let mut trace = None;
        let mut conversation = None;
        let mut handoff = HandoffReport::default();

        while let Some(event) = stream.next().await {
            match event {
                Ok(A2aStreamEvent::StatusUpdate {
                    task_id,
                    state,
                    message,
                    metadata,
                    ..
                }) => {
                    // The worker's spend rides on its terminal status; a
                    // failed task spent its tokens too (AGE-415).
                    let reported = usage_from_status_metadata(metadata.as_ref());
                    if !reported.is_empty() {
                        usage = reported
                            .into_iter()
                            .map(|line| TokenUsage {
                                delegated_to: Some(config.name.clone()),
                                ..line
                            })
                            .collect();
                    }
                    if state == "failed" || state == "completed" {
                        // The handoff rides the terminal status either way
                        // (TD-2, AGE-693).
                        handoff = HandoffReport::from_status_metadata(metadata.as_ref());
                    }
                    if state == "failed" {
                        success = false;
                        error_msg = message.clone();
                    } else if state == "working"
                        && let Some(ref msg) = message
                    {
                        self.send_progress(InvokeAgentProgress::Step(msg.clone()));
                    } else if state == "input-required"
                        && let Some(request) =
                            A2aClarificationRequest::from_status_metadata(metadata.as_ref())
                        && let Err(e) = self.answer_input_required(config, &task_id, request).await
                    {
                        // Dropping the stream on the way out cancels the
                        // parked task, which reaps the worker.
                        success = false;
                        error_msg = Some(e);
                        break;
                    } else if state == "completed" {
                        if include_trace {
                            // The worker's trace rides the same terminal
                            // status as its usage (AGE-467); a failed task
                            // never reaches this branch, so it never
                            // returns one.
                            trace = trace_from_status_metadata(metadata.as_ref());
                        }
                        // The worker's captured conversation rides the same
                        // terminal status too (RC-0, AGE-649), read back
                        // unconditionally: unlike the trace, capture is a
                        // broker-side decision this call has no argument
                        // for, so there is nothing here to gate it on.
                        conversation = conversation_from_status_metadata(metadata.as_ref());
                    }
                    // An "input-required" without a request is an approval
                    // the worker is waiting on, which the worker settles
                    // itself; nothing to do here.
                }
                Ok(A2aStreamEvent::ArtifactUpdate { text, .. }) => {
                    if !text.is_empty() {
                        self.send_progress(InvokeAgentProgress::Text(text.clone()));
                        response.push_str(&text);
                    }
                }
                Err(e) => {
                    warn!(agent = %config.name, error = %e, "Stream error");
                    success = false;
                    error_msg = Some(e.to_string());
                    break;
                }
            }
        }

        self.finish(
            &config.name,
            response,
            success,
            error_msg,
            usage,
            trace,
            conversation,
            handoff,
        )
    }

    /// Delegate to local role `agent` over `transport` (ADR-0020, BI-4):
    /// the same progress, answer and failures as the A2A path, from the
    /// broker's call frames instead of an SSE stream.
    async fn call_over_fabric(
        &self,
        transport: &dyn Transport,
        agent: &str,
        prompt: &str,
        include_trace: bool,
    ) -> Result<InvokeAgentOutput, InvokeAgentError> {
        use futures::StreamExt;

        let request = CallRequest::InvokeAgent(InvokeAgentParams {
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            handle: None,
            include_trace,
            spawn_context: None,
        });
        let mut stream = transport.call(request).await.map_err(|e| {
            let err_text = format!("\u{26a0}\u{fe0f} Failed to invoke agent '{agent}': {e}");
            self.send_progress(InvokeAgentProgress::Finished {
                success: false,
                result: Some(err_text),
                usage: Vec::new(),
            });
            InvokeAgentError::InvocationFailed(format!("Failed to invoke agent '{agent}': {e}"))
        })?;

        let mut outcome = None;
        let mut failure = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(CallEvent::Progress(value)) => {
                    if let Some(progress) = progress_from_value(value) {
                        self.send_progress(progress);
                    }
                }
                Ok(CallEvent::InputRequired { task, request }) => {
                    let answered = match serde_json::from_value::<A2aClarificationRequest>(request)
                    {
                        Ok(request) => {
                            self.answer_over_fabric(transport, agent, &task, request)
                                .await
                        }
                        // A question with nothing to answer is an approval
                        // the worker settles itself, as on the A2A path.
                        Err(_) => Ok(()),
                    };
                    if let Err(e) = answered {
                        // Dropping the stream on the way out cancels the
                        // call, which reaps the worker.
                        failure = Some(e);
                        break;
                    }
                }
                Ok(CallEvent::Result(value)) => {
                    outcome = Some(serde_json::from_value::<InvokeAgentOutcome>(value).map_err(
                        |e| format!("the broker's result for '{agent}' did not parse: {e}"),
                    ));
                    break;
                }
                Err(CallError::Delegation(refusal)) => {
                    // A refusal is the tool's error, not a failed task: the
                    // model reads the typed reason, as it does `CapExceeded`.
                    warn!(agent = %agent, %refusal, "The broker refused the delegation");
                    self.send_progress(InvokeAgentProgress::Finished {
                        success: false,
                        result: Some(format!("\u{26a0}\u{fe0f} {refusal}")),
                        usage: Vec::new(),
                    });
                    return Err(InvokeAgentError::Refused(refusal));
                }
                Err(e) => {
                    warn!(agent = %agent, error = %e, "Fabric call failed");
                    failure = Some(e.to_string());
                    break;
                }
            }
        }

        let outcome = match (failure, outcome) {
            (Some(reason), _) | (None, Some(Err(reason))) => InvokeAgentOutcome {
                success: false,
                response: String::new(),
                error: Some(reason),
                metadata: None,
            },
            (None, Some(Ok(outcome))) => outcome,
            (None, None) => InvokeAgentOutcome {
                success: false,
                response: String::new(),
                error: Some("the broker ended the call without a result".to_string()),
                metadata: None,
            },
        };

        let metadata = outcome.metadata.as_ref();
        let handoff = HandoffReport::from_status_metadata(metadata);
        // The worker's spend rides on its terminal status; a failed task
        // spent its tokens too (AGE-415).
        let usage = usage_from_status_metadata(metadata)
            .into_iter()
            .map(|line| TokenUsage {
                delegated_to: Some(agent.to_string()),
                ..line
            })
            .collect();
        let (trace, conversation) = if outcome.success {
            (
                include_trace
                    .then(|| trace_from_status_metadata(metadata))
                    .flatten(),
                conversation_from_status_metadata(metadata),
            )
        } else {
            (None, None)
        };
        self.finish(
            agent,
            outcome.response,
            outcome.success,
            outcome.error,
            usage,
            trace,
            conversation,
            handoff,
        )
    }

    /// Report how a delegation ended, to the progress channel and to the
    /// model — the one ending both the A2A and the fabric path share.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        agent: &str,
        response: String,
        success: bool,
        error_msg: Option<String>,
        usage: Vec<TokenUsage>,
        trace: Option<String>,
        conversation: Option<serde_json::Value>,
        handoff: HandoffReport,
    ) -> Result<InvokeAgentOutput, InvokeAgentError> {
        let response = response.trim().to_string();
        let handoff_value = if success { handoff.handoff } else { None };
        if let Some(ledger) = self.handoff_ledger.as_ref() {
            ledger.record(agent, handoff.invalid_count, handoff_value.as_ref());
        }

        if !success {
            let err_text = error_msg
                .as_ref()
                .map(|m| format!("⚠️ {m}"))
                .unwrap_or_else(|| "⚠️ Agent failed".to_string());
            self.send_progress(InvokeAgentProgress::Finished {
                success: false,
                result: Some(err_text),
                usage,
            });
            if let Some((role, errors)) = handoff.invalid {
                return Err(InvokeAgentError::HandoffInvalid { role, errors });
            }
            return Err(InvokeAgentError::InvocationFailed(format!(
                "Agent '{}' reported failure{}",
                agent,
                error_msg.map(|m| format!(": {}", m)).unwrap_or_default()
            )));
        }

        // Emit Finished with the full result so the sub-agent trace block
        // shows the response (identical to /agent visualisation).
        self.send_progress(InvokeAgentProgress::Finished {
            success: true,
            result: if response.is_empty() {
                None
            } else {
                Some(response.clone())
            },
            usage,
        });

        debug!(agent = %agent, response_len = response.len(), "Agent responded");

        // Return the actual agent output to the parent model as well. This keeps
        // the sub-agent trace useful for transparency while still giving the
        // calling model the concrete result it needs to answer correctly.
        Ok(InvokeAgentOutput {
            agent: agent.to_string(),
            response: if response.is_empty() {
                format!("Agent '{}' completed successfully.", agent)
            } else {
                response
            },
            success: true,
            trace,
            conversation,
            handoff: handoff_value,
        })
    }
}

impl InvokeAgentTool {
    /// The delegated task asked a question: get it answered and send the
    /// answer back down on the same task.
    ///
    /// `Err` is the reason the delegation cannot continue, worded for the
    /// model.
    async fn answer_input_required(
        &self,
        config: &A2aAgentConfig,
        task_id: &str,
        request: A2aClarificationRequest,
    ) -> Result<(), String> {
        let request_id = request.id.clone();
        let answers = self.ask(&config.name, task_id, request).await?;
        self.client
            .send_task_input(config, task_id, &request_id, &answers)
            .await
            .map_err(|e| undeliverable(&config.name, e))
    }

    /// As [`answer_input_required`](Self::answer_input_required), with the
    /// answer going back down over the fabric the call came over.
    async fn answer_over_fabric(
        &self,
        transport: &dyn Transport,
        agent: &str,
        task_id: &str,
        request: A2aClarificationRequest,
    ) -> Result<(), String> {
        let request_id = request.id.clone();
        let answers = self.ask(agent, task_id, request).await?;
        let input = serde_json::json!({ "requestId": request_id, "answers": answers });
        transport
            .answer(task_id, input)
            .await
            .map_err(|e| undeliverable(agent, e))
    }

    /// Re-ask a delegated agent's questions on this agent's own store and
    /// wait for the answers (AGE-306).
    async fn ask(
        &self,
        agent: &str,
        task_id: &str,
        request: A2aClarificationRequest,
    ) -> Result<Vec<crate::models::clarification_store::ClarificationAnswer>, String> {
        let Some(pending) = self.clarifications.as_ref() else {
            return Err(format!(
                "Agent '{}' asked a question and nobody here can answer it: {}",
                agent,
                request
                    .questions
                    .first()
                    .map(|q| q.question.as_str())
                    .unwrap_or("(no question text)")
            ));
        };

        info!(
            agent = %agent,
            task = %task_id,
            request = %request.id,
            questions = request.questions.len(),
            "Delegated agent asked a question; escalating"
        );
        request_clarification(pending, request.questions)
            .await
            .map_err(|e| format!("Agent '{agent}' asked a question that went unanswered: {e}"))
    }
}

/// Why an answer never reached the agent that asked for it.
fn undeliverable(agent: &str, error: impl std::fmt::Display) -> String {
    format!("Agent '{agent}' asked a question, but the answer could not be delivered: {error:#}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::a2a_store::A2aAgentConfig;

    fn make_agent(name: &str, url: &str, enabled: bool) -> A2aAgentConfig {
        A2aAgentConfig {
            name: name.to_string(),
            url: url.to_string(),
            api_key: None,
            enabled,
            skills: vec![],
        }
    }

    /// Collect the progress the tool reports, as the frontend's channel does.
    fn watch_progress(tool: &InvokeAgentTool) -> std::sync::mpsc::Receiver<InvokeAgentProgress> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel();
        *tool.progress_slot().lock() = Some(async_tx);
        tokio::spawn(async move {
            while let Some(event) = async_rx.recv().await {
                let _ = tx.send(event);
            }
        });
        rx
    }

    fn warnings(rx: &std::sync::mpsc::Receiver<InvokeAgentProgress>) -> Vec<String> {
        rx.try_iter()
            .filter_map(|event| match event {
                InvokeAgentProgress::Text(text) if text.contains("outside your fleet") => {
                    Some(text)
                }
                _ => None,
            })
            .collect()
    }

    /// ADR-0011 C5: with the flag on, a prompt leaving this user's machines is
    /// said out loud before it goes. The call itself then fails — there is no
    /// server at that URL — which is fine: the warning has to come first.
    #[tokio::test]
    async fn a_third_party_agent_is_announced_before_the_prompt_goes() {
        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            None,
        )
        .with_external_agent_warning(true);
        let progress = watch_progress(&tool);

        let _ = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                    include_trace: false,
                },
            )
            .await;

        // Give the forwarding task a moment to drain the channel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let said = warnings(&progress);
        assert_eq!(said.len(), 1, "expected exactly one warning, got {said:?}");
        assert!(said[0].contains("voucher"), "{}", said[0]);
        assert!(said[0].contains("127.0.0.1:1"), "{}", said[0]);
        assert!(
            said[0].contains("remote_configured"),
            "the warning names the origin: {}",
            said[0]
        );
    }

    /// The flag is off by default, and the hook is silent until someone turns
    /// it on. Which trust policy it should carry is not decided here.
    #[tokio::test]
    async fn nothing_is_said_when_the_flag_is_off() {
        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            None,
        );
        let progress = watch_progress(&tool);

        let _ = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                    include_trace: false,
                },
            )
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(warnings(&progress).is_empty());
    }

    /// AGE-416: a leader over its cap is refused before anything starts —
    /// no `Started` progress, no HTTP call (the agent's URL has no server,
    /// so reaching it would have produced a different, network error) —
    /// with the typed error the model reads and the transcript renders.
    #[tokio::test]
    async fn a_spent_cap_refuses_the_delegation_before_it_starts() {
        use crate::services::spend_gate::FixedSpendGate;

        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            None,
        )
        .with_spend_gate(Arc::new(FixedSpendGate::refusing(12.5, 10.0)));
        let progress = watch_progress(&tool);

        let err = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                    include_trace: false,
                },
            )
            .await
            .expect_err("the delegation is refused");

        let InvokeAgentError::CapExceeded(ref refused) = err else {
            panic!("expected CapExceeded, got {err:?}");
        };
        assert_eq!(refused.month_to_date_usd, 12.5);
        assert_eq!(refused.cap_usd, 10.0);
        assert_eq!(
            err.to_string(),
            "cap_exceeded: month-to-date $12.50 \u{2265} cap $10.00; no delegation started"
        );

        // What the model and the transcript see: the tool's error path, with
        // the load-bearing `Error:` prefix and the structured source intact.
        let mapped = tool.map_error(err);
        assert_eq!(
            mapped.model_feedback().unwrap_or_default(),
            "Error: invoke_agent: cap_exceeded: month-to-date $12.50 \u{2265} cap $10.00; \
             no delegation started"
        );
        let source = mapped
            .downcast_ref::<InvokeAgentError>()
            .expect("the typed error survives into the tool result");
        let InvokeAgentError::CapExceeded(refused) = source else {
            panic!("expected CapExceeded, got {source:?}");
        };
        assert_eq!(
            serde_json::to_value(refused).unwrap(),
            serde_json::json!({
                "error": "cap_exceeded",
                "month_to_date": 12.5,
                "cap_usd": 10.0,
            })
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let events: Vec<InvokeAgentProgress> = progress.try_iter().collect();
        assert!(
            events.is_empty(),
            "nothing started, so nothing was reported: {events:?}"
        );
    }

    /// A gate that permits changes nothing: the call goes on to the agent
    /// exactly as it would without a gate (and fails there, since there is
    /// no server — a network failure, not a refusal).
    #[tokio::test]
    async fn a_gate_under_the_cap_lets_the_delegation_through() {
        use crate::services::spend_gate::FixedSpendGate;

        let tool = InvokeAgentTool::new(
            vec![make_agent("voucher", "http://127.0.0.1:1/a2a", true)],
            None,
        )
        .with_spend_gate(Arc::new(FixedSpendGate::permitting()));
        let progress = watch_progress(&tool);

        let err = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "voucher".to_string(),
                    prompt: "summarise the contract".to_string(),
                    include_trace: false,
                },
            )
            .await
            .expect_err("there is no server at that URL");

        assert!(
            matches!(err, InvokeAgentError::InvocationFailed(_)),
            "the call reached the agent, got {err:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            progress
                .try_iter()
                .any(|e| matches!(e, InvokeAgentProgress::Started { .. })),
            "the delegation was started"
        );
    }

    #[tokio::test]
    async fn test_invoke_not_found() {
        let tool = InvokeAgentTool::new(vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "nonexistent".to_string(),
                    prompt: "hello".to_string(),
                    include_trace: false,
                },
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, InvokeAgentError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_invoke_disabled_remote_agent() {
        let agent = make_agent("my-agent", "https://example.com/a2a", false);
        let tool = InvokeAgentTool::new(vec![agent], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "my-agent".to_string(),
                    prompt: "hello".to_string(),
                    include_trace: false,
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }

    /// PL-U5: a WASM module is never an agent. A name only a module
    /// carries is not found, and the error lists the roster's specs.
    #[tokio::test]
    async fn a_module_name_is_not_an_agent_and_the_roster_is_offered() {
        let tool = InvokeAgentTool::new(vec![], None)
            .with_local_agents(crate::agent_spec::roster_names_from(&[], None, None));

        let err = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "benford".to_string(),
                    prompt: "analyze data".to_string(),
                    include_trace: false,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, InvokeAgentError::NotFound(_)), "{err}");
        assert!(err.to_string().contains("benford-analyst"), "{err}");
    }

    /// A roster spec is reached through the broker, which this tool needs:
    /// without one it says so rather than falling through to anything.
    #[tokio::test]
    async fn a_roster_spec_needs_the_broker() {
        let tool = InvokeAgentTool::new(vec![], None).with_local_agents(["benford-analyst"]);

        let err = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "benford-analyst".to_string(),
                    prompt: "1, 2, 3".to_string(),
                    include_trace: false,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, InvokeAgentError::InvocationFailed(_)),
            "{err}"
        );
        assert!(err.to_string().contains("protocol gateway"), "{err}");
    }

    #[tokio::test]
    async fn test_invoke_empty_agent_name() {
        let tool = InvokeAgentTool::new(vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "  ".to_string(),
                    prompt: "hello".to_string(),
                    include_trace: false,
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_invoke_empty_prompt() {
        let tool = InvokeAgentTool::new(vec![], None);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "some-agent".to_string(),
                    prompt: "".to_string(),
                    include_trace: false,
                },
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            InvokeAgentError::InvocationFailed(_)
        ));
    }

    #[tokio::test]
    async fn test_remote_agent_takes_precedence() {
        // When a remote agent and a local spec share a name, remote wins
        let remote = make_agent("shared-name", "https://example.com/a2a", false);
        let tool =
            InvokeAgentTool::new(vec![remote], Some(8420)).with_local_agents(["shared-name"]);

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: "shared-name".to_string(),
                    prompt: "hello".to_string(),
                    include_trace: false,
                },
            )
            .await;

        // Should hit the remote disabled check, not the local path
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InvokeAgentError::Disabled(_)));
    }

    /// A fabric that refuses every call with one [`Refusal`].
    struct Refusing(Refusal);

    #[async_trait::async_trait]
    impl Transport for Refusing {
        async fn call(&self, _req: CallRequest) -> Result<chatty_fabric::CallStream, CallError> {
            use futures::StreamExt;
            Ok(futures::stream::iter([Err(CallError::Delegation(self.0.clone()))]).boxed())
        }
    }

    /// DP-2: a broker's refusal reaches the model as the tool's error, with
    /// the exact text of each reason after the `Error: invoke_agent: `
    /// prefix, and the typed refusal survives as the error's source.
    #[tokio::test]
    async fn refusal_text_golden() {
        let s = |v: &str| v.to_string();
        for (refusal, text) in [
            (
                Refusal::Cycle {
                    chain: vec![s("coordinator"), s("reviewer")],
                    callee: s("coordinator"),
                },
                "Error: invoke_agent: cycle: coordinator \u{2192} reviewer \u{2192} coordinator",
            ),
            (
                Refusal::TooDeep { depth: 5, max: 4 },
                "Error: invoke_agent: too_deep: depth 5 > max 4",
            ),
            (
                Refusal::NotListed {
                    caller: s("reviewer"),
                    callee: s("data-coder"),
                },
                "Error: invoke_agent: not_listed: reviewer may not call data-coder",
            ),
            (
                Refusal::NotExposed {
                    callee: s("data-coder"),
                },
                "Error: invoke_agent: not_exposed: data-coder is not exposed",
            ),
            (
                Refusal::CallerNotAllowed {
                    caller: s("reviewer"),
                    callee: s("data-coder"),
                },
                "Error: invoke_agent: caller_not_allowed: data-coder does not accept calls \
                 from reviewer",
            ),
        ] {
            let tool = InvokeAgentTool::new(vec![], vec![], None)
                .with_local_agents(vec![s("data-coder")])
                .with_transport(Arc::new(Refusing(refusal.clone())));
            let progress = watch_progress(&tool);
            let err = tool
                .call(
                    &mut ToolContext::new(),
                    InvokeAgentArgs {
                        agent: s("data-coder"),
                        prompt: s("fix it"),
                        include_trace: false,
                    },
                )
                .await
                .expect_err("the broker refused");
            let mapped = tool.map_error(err);
            assert_eq!(mapped.model_feedback().unwrap_or_default(), text);
            let source = mapped
                .downcast_ref::<InvokeAgentError>()
                .expect("the typed error survives into the tool result");
            assert!(
                matches!(source, InvokeAgentError::Refused(r) if *r == refusal),
                "{source:?}"
            );
            tokio::task::yield_now().await;
            let events: Vec<_> = progress.try_iter().collect();
            assert!(
                matches!(
                    events.last(),
                    Some(InvokeAgentProgress::Finished { success: false, .. })
                ),
                "the transcript's card ends failed: {events:?}"
            );
        }
    }
}
