//! A2A (Agent-to-Agent) HTTP client.
//!
//! Implements the client side of the A2A protocol:
//! - `GET /.well-known/agent.json` — discover the agent's capabilities
//! - `POST <url>` with `message/send` JSON-RPC — send a task and receive a result
//! - `POST <url>` with `message/stream` JSON-RPC — stream task updates via SSE
//! - `POST <url>` with `message/send` on an existing `taskId` — answer a task
//!   parked in `input-required` (ADR-0011 C7, AGE-306)

use anyhow::{Context, Result, bail};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::models::clarification_store::{
    CLARIFICATION_TIMEOUT, ClarificationAnswer, ClarifyingQuestion,
};
use crate::models::token_usage::{ModelRef, TokenUsage};
use crate::services::ssrf_guard::{
    AddressPolicy, GuardedResolver, HostLookup, SystemLookup, a2a_peer, a2a_peer_with_bypass,
    check_a2a_url_without_lookup,
};
use crate::settings::models::a2a_store::A2aAgentConfig;
use chatty_fabric::wire::{WireModelRef, WireUsage, WireUsageLine};

/// The key under a status's `metadata` that carries what an
/// `input-required` task is waiting for, and under an answering message's
/// `metadata` that carries the answers. The broker's spelling
/// (`chatty_protocol_gateway::handlers`), repeated here because this crate
/// is below the gateway.
pub const CLARIFICATION_METADATA_KEY: &str = "clarification";

/// What an A2A peer's task parked in `input-required` is waiting for: its
/// questions, with the request id its answer must name.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct A2aClarificationRequest {
    pub id: String,
    pub questions: Vec<ClarifyingQuestion>,
}

impl A2aClarificationRequest {
    /// The request behind an `input-required` status, if the status carries
    /// one. A parked task without it — an approval, say — cannot be answered
    /// from here.
    pub fn from_status_metadata(metadata: Option<&Value>) -> Option<Self> {
        let value = metadata?.get(CLARIFICATION_METADATA_KEY)?.clone();
        serde_json::from_value(value).ok()
    }
}

/// The key under a terminal status's `metadata` that carries the worker's
/// token usage, as the broker's worker mapper writes it
/// (`chatty_protocol_gateway::worker::mapper`). A2A has no notion of usage;
/// this is the one place it rides (ADR-0011).
pub const USAGE_METADATA_KEY: &str = "usage";

/// `metadata.usage` for a terminal status, as the wire carries it
/// ([`WireUsage`]): the four token totals, and `lines`, one per model
/// (AGE-682). Nothing on it is a price.
pub fn wire_usage(lines: &[TokenUsage]) -> WireUsage {
    WireUsage::from_lines(lines.iter().map(WireUsageLine::from).collect())
}

/// The lines of a usage report off the wire, one [`TokenUsage`] each. A
/// line naming a provider this build does not know is logged and skipped,
/// not the whole report.
pub fn usage_from_wire(usage: &WireUsage) -> Vec<TokenUsage> {
    usage.lines.iter().cloned().filter_map(usage_line).collect()
}

fn usage_line(line: WireUsageLine) -> Option<TokenUsage> {
    TokenUsage::try_from(line)
        .map_err(|e| warn!(error = %e, "Skipping a usage line that does not parse"))
        .ok()
}

impl From<&TokenUsage> for WireUsageLine {
    fn from(line: &TokenUsage) -> Self {
        Self {
            model: line.model.as_ref().map(WireModelRef::from),
            input_tokens: line.input_tokens,
            output_tokens: line.output_tokens,
            cache_read_tokens: line.cache_read_tokens,
            cache_write_tokens: line.cache_write_tokens,
            at: line
                .at
                .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64),
            duration_ms: line.duration_ms,
        }
    }
}

/// An error for a line whose provider this build does not know.
impl TryFrom<WireUsageLine> for TokenUsage {
    type Error = serde_json::Error;

    fn try_from(line: WireUsageLine) -> Result<Self, Self::Error> {
        Ok(Self {
            input_tokens: line.input_tokens,
            output_tokens: line.output_tokens,
            cache_read_tokens: line.cache_read_tokens,
            cache_write_tokens: line.cache_write_tokens,
            model: line.model.as_ref().map(ModelRef::from_wire).transpose()?,
            at: line.at.map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            duration_ms: line.duration_ms,
            ..TokenUsage::default()
        })
    }
}

/// The usage a delegated task reports on its terminal status:
/// `metadata.usage.lines`, one [`TokenUsage`] per model, each naming its
/// model (AGE-682). Whatever the worker itself delegated is already folded
/// in by its mapper, so this is the whole subtree (AGE-415). A line that does
/// not parse is logged and skipped, not the whole report; a report with the
/// four totals but no `lines` is one line naming no model, so it counts,
/// unpriced. Empty when the status carries no usage.
pub fn usage_from_status_metadata(metadata: Option<&Value>) -> Vec<TokenUsage> {
    let Some(usage) = metadata.and_then(|m| m.get(USAGE_METADATA_KEY)) else {
        return Vec::new();
    };
    let Some(lines) = usage.get("lines").and_then(Value::as_array) else {
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
        let totals = TokenUsage {
            input_tokens: count("inputTokens"),
            output_tokens: count("outputTokens"),
            cache_read_tokens: count("cacheReadTokens"),
            cache_write_tokens: count("cacheWriteTokens"),
            ..TokenUsage::default()
        };
        let spent = totals.input_tokens
            + totals.output_tokens
            + totals.cache_read_tokens
            + totals.cache_write_tokens;
        return if spent == 0 { Vec::new() } else { vec![totals] };
    };
    lines
        .iter()
        .filter_map(|line| {
            serde_json::from_value::<WireUsageLine>(line.clone())
                .map_err(|e| warn!(error = %e, "Skipping a usage line that does not parse"))
                .ok()
        })
        .filter_map(usage_line)
        .collect()
}

/// The key under a terminal status's `metadata` that carries the worker's
/// compacted tool-call trace, as the broker's worker mapper writes it
/// (`chatty_protocol_gateway::worker::mapper`). Rides next to `usage`
/// (AGE-467): a leader that wants to judge a worker's derivation, not just
/// its answer, asks `invoke_agent` for it with `include_trace`.
pub const TRACE_METADATA_KEY: &str = "trace";

/// The trace a delegated task's terminal status carries, if any: the
/// compacted string the worker's mapper wrote under
/// [`TRACE_METADATA_KEY`]. `None` when the metadata carries no trace — every
/// delegation that did not ask for one, and any task that made no tool
/// calls.
pub fn trace_from_status_metadata(metadata: Option<&Value>) -> Option<String> {
    metadata?
        .get(TRACE_METADATA_KEY)?
        .as_str()
        .map(str::to_string)
}

/// The key under a terminal status's `metadata` that carries the worker's
/// captured conversation (RC-0, AGE-649): the messages of every turn the
/// task ran, in order, as the broker's worker mapper writes them
/// (`chatty_protocol_gateway::worker::mapper`). Opt-in per task; absent when
/// the task never asked for it.
pub const CONVERSATION_METADATA_KEY: &str = "conversation";

/// The key a terminal status carries instead of [`CONVERSATION_METADATA_KEY`]
/// when the captured conversation is over the spec's 32 MB cap: the byte
/// count of what was not sent, so a caller sees why rather than getting a
/// silently truncated conversation.
pub const CONVERSATION_TOO_LARGE_METADATA_KEY: &str = "conversationTooLarge";

/// The conversation a delegated task's terminal status carries, if any: the
/// JSON array of messages the worker's mapper wrote under
/// [`CONVERSATION_METADATA_KEY`]. `None` when the task did not capture one,
/// or when it was too large ([`conversation_too_large_from_status_metadata`]).
pub fn conversation_from_status_metadata(metadata: Option<&Value>) -> Option<Value> {
    metadata?.get(CONVERSATION_METADATA_KEY).cloned()
}

/// The byte count a delegated task's terminal status carries under
/// [`CONVERSATION_TOO_LARGE_METADATA_KEY`], if its captured conversation
/// went over the cap.
pub fn conversation_too_large_from_status_metadata(metadata: Option<&Value>) -> Option<u64> {
    metadata?.get(CONVERSATION_TOO_LARGE_METADATA_KEY)?.as_u64()
}

/// Discovered capabilities from a remote A2A agent card.
#[derive(Clone, Debug)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
    /// Whether the remote agent supports the `message/stream` method.
    pub supports_streaming: bool,
}

/// An event received from an A2A `message/stream` SSE response.
#[derive(Clone, Debug)]
pub enum A2aStreamEvent {
    /// Task status changed (e.g. "working", "completed", "failed").
    StatusUpdate {
        task_id: String,
        state: String,
        is_final: bool,
        /// Optional status message (e.g. progress text or error details).
        message: Option<String>,
        /// The status's `metadata`, verbatim. Carries the request behind an
        /// `input-required` state (see [`A2aClarificationRequest`]) and the
        /// worker's usage on the terminal status.
        metadata: Option<Value>,
    },
    /// An artifact chunk (text content from the agent).
    ArtifactUpdate {
        task_id: String,
        text: String,
        last_chunk: bool,
    },
}

/// How long the delegation transport waits for the far side to connect.
///
/// Short on purpose: an agent that cannot be reached at all should fail
/// quickly, whatever the rest of the exchange is allowed to take.
pub const DELEGATION_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a delegation stream may stay **silent** before it is treated as
/// dead (ADR-0011 C7, AGE-319).
///
/// Not a budget for the delegation: a delegated turn can legitimately run for
/// as long as its work takes, and a worker's question can park it for as long
/// as a human takes to answer. What cannot be legitimate is a stream that
/// says nothing at all — this app's broker sends SSE keep-alives every 15 s,
/// so silence past this point means the socket, not the agent, has gone.
///
/// It exceeds [`CLARIFICATION_TIMEOUT`] deliberately, and the assertion below
/// keeps it that way: an unanswered question has to fail as an unanswered
/// question, on the clarification store's own deadline, rather than as a
/// transport error that says nothing about what happened.
pub const DELEGATION_READ_TIMEOUT: Duration = Duration::from_secs(360);

const _: () = assert!(
    DELEGATION_READ_TIMEOUT.as_secs() > CLARIFICATION_TIMEOUT.as_secs(),
    "the delegation transport must outlast the clarification timeout, or a \
     question nobody answers fails as a dead socket instead"
);

/// The largest single SSE event, or whole JSON response body, an A2A peer
/// may send. A terminal status can carry the worker's conversation (up to
/// the gateway's 32 MiB capture cap) plus its envelope; this matches the
/// interim 33 MiB frame cap of ADR-0021 step 0.
pub const A2A_MAX_EVENT_BYTES: usize = 33 * 1024 * 1024;

/// The most an A2A peer may send over one `message/stream` in total.
pub const A2A_MAX_STREAM_BYTES: usize = 256 * 1024 * 1024;

/// How much of an error response's body is kept for the error message.
const ERROR_BODY_BYTES: usize = 64 * 1024;

/// What an A2A peer may send back (ADR-0021 step 0, AGE-767). Exceeding any
/// bound fails the call with an error that names the bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct A2aLimits {
    /// One SSE event, or a whole non-streaming JSON body.
    pub max_event_bytes: usize,
    /// Everything one SSE stream delivers.
    pub max_stream_bytes: usize,
    /// How long an SSE stream may deliver nothing at all.
    pub idle_timeout: Duration,
}

impl Default for A2aLimits {
    fn default() -> Self {
        Self {
            max_event_bytes: A2A_MAX_EVENT_BYTES,
            max_stream_bytes: A2A_MAX_STREAM_BYTES,
            idle_timeout: DELEGATION_READ_TIMEOUT,
        }
    }
}

/// An HTTP client for remote A2A agents: the A2A edge.
///
/// Every connection it makes — agent card, `message/send`, `message/stream`
/// — goes through the SSRF guard's [`GuardedResolver`] with the
/// [`a2a_peer`] policy: a name is resolved once, the answer is checked, and
/// the connection goes to exactly the checked addresses; a lookup that
/// fails is a refusal. It never follows a redirect, never uses a proxy,
/// bounds what a peer may send ([`A2aLimits`]), and sends no credential but
/// the one configured for the agent it calls (ADR-0021 step 0, AGE-767).
///
/// Two `reqwest::Client`s share everything but their `GuardedResolver`
/// policy: `http` is [`a2a_peer`] (the default, no private ranges), and
/// `http_private_network` is [`a2a_peer_with_bypass`]`(true)`, used only for
/// a call whose [`A2aAgentConfig::allow_private_network`] is set (AGE-806).
/// Reqwest's DNS resolver is fixed per `Client`, so the per-agent opt-in
/// picks between the two rather than reconfiguring one.
#[derive(Clone)]
pub struct A2aClient {
    http: reqwest::Client,
    http_private_network: reqwest::Client,
    limits: A2aLimits,
}

/// The one way the edge's HTTP client is built.
fn edge_http(
    lookup: Arc<dyn HostLookup>,
    policy: AddressPolicy,
    timeouts: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
) -> reqwest::Client {
    timeouts(reqwest::Client::builder())
        .user_agent(crate::services::http_client::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        // A proxy would resolve the target itself, out of the guard's sight.
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver::new(lookup, policy)))
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

fn delegation_timeouts(
    read: Duration,
) -> impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    move |builder| {
        builder
            .connect_timeout(DELEGATION_CONNECT_TIMEOUT)
            .read_timeout(read)
    }
}

/// Refuse a URL the edge must not call before anything is sent: plain HTTP
/// off the local network (AGE-756, unchanged) and an IP literal the
/// [`a2a_peer`] policy refuses (a literal never reaches the resolver).
fn ensure_callable(url: &str) -> Result<()> {
    hive_client::ensure_secure_url(url).map_err(|e| anyhow::anyhow!(e))?;
    check_a2a_url_without_lookup(url).map_err(|e| anyhow::anyhow!(e))
}

/// Attach the agent's own credential, and nothing else.
fn authorized(req: reqwest::RequestBuilder, config: &A2aAgentConfig) -> reqwest::RequestBuilder {
    match config.api_key.as_deref().filter(|k| !k.is_empty()) {
        Some(key) => req.bearer_auth(key),
        None => req,
    }
}

/// Send `req` and accept only a direct success: a redirect is an error,
/// never followed, so neither the request nor the credential goes anywhere
/// but the configured agent.
async fn send_checked(
    req: reqwest::RequestBuilder,
    url: &str,
    operation: &str,
) -> Result<reqwest::Response> {
    let resp = req
        .send()
        .await
        .with_context(|| format!("Failed to reach A2A agent at {}", url))?;
    let status = resp.status();
    if status.is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("nowhere");
        bail!(
            "A2A {operation} to {url} was redirected ({status}) to {location}; \
             A2A requests do not follow redirects"
        );
    }
    if !status.is_success() {
        bail!("{}", detailed_http_error(operation, resp).await);
    }
    Ok(resp)
}

/// Read a whole response body, refusing one larger than `max` bytes.
async fn read_capped(mut resp: reqwest::Response, max: usize, what: &str) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .with_context(|| format!("Failed to read the A2A {what}"))?
    {
        if body.len() + chunk.len() > max {
            bail!("the A2A {what} exceeds the {max}-byte cap; refusing it");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_json_capped(resp: reqwest::Response, max: usize, what: &str) -> Result<Value> {
    let body = read_capped(resp, max, what).await?;
    serde_json::from_slice(&body).with_context(|| format!("Failed to parse the A2A {what} as JSON"))
}

async fn detailed_http_error(operation: &str, mut resp: reqwest::Response) -> String {
    let status = resp.status();
    let mut prefix = Vec::new();
    while prefix.len() < ERROR_BODY_BYTES
        && let Ok(Some(chunk)) = resp.chunk().await
    {
        prefix.extend_from_slice(&chunk);
    }
    prefix.truncate(ERROR_BODY_BYTES);
    let body = String::from_utf8_lossy(&prefix);
    let body = body.trim();

    let mismatch_hint = if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
        && (body.contains("missing field `task`")
            || body.contains("missing field `taskId`")
            || body.contains("Failed to deserialize"))
    {
        " (runner likely still uses the old non-JSON-RPC A2A handler; rebuild/restart hive-runner)"
    } else {
        ""
    };

    if body.is_empty() {
        format!(
            "A2A {} failed with status {}{}",
            operation, status, mismatch_hint
        )
    } else {
        format!(
            "A2A {} failed with status {}: {}{}",
            operation, status, body, mismatch_hint
        )
    }
}

/// What a finished `message/send` task amounts to: its text, or an error
/// carrying why it failed.
///
/// A task that failed used to come back as `Ok("")` — the state was never
/// read, so every failure reached the caller as an empty success. That is
/// silence in place of a reason, and under AGE-321 it is specifically the
/// reason a worker's unanswerable question would never be seen: the broker
/// puts the question in the failure's message, and this is what carries it
/// out.
fn task_outcome(value: &Value) -> Result<String> {
    let state = value
        .pointer("/result/status/state")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if matches!(state, "failed" | "canceled") {
        let reason = value
            .pointer("/result/status/message/parts/0/text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("the agent gave no reason");
        bail!("the agent's task {state}: {reason}");
    }

    Ok(value
        .pointer("/result/artifacts/0/parts/0/text")
        .or_else(|| value.pointer("/result/artifacts/0/parts/0"))
        .and_then(|v| v.as_str())
        .or_else(|| value.pointer("/result/output").and_then(|v| v.as_str()))
        .or_else(|| value.pointer("/result").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string())
}

impl A2aClient {
    pub fn new() -> Self {
        let lookup: Arc<dyn HostLookup> = Arc::new(SystemLookup);
        Self {
            http: edge_http(Arc::clone(&lookup), Arc::new(a2a_peer), |builder| {
                builder.timeout(Duration::from_secs(30))
            }),
            http_private_network: edge_http(lookup, a2a_peer_with_bypass(true), |builder| {
                builder.timeout(Duration::from_secs(30))
            }),
            limits: A2aLimits::default(),
        }
    }

    /// A client for driving a delegation stream.
    ///
    /// Bounds silence rather than duration — see [`DELEGATION_READ_TIMEOUT`].
    pub fn for_delegation() -> Self {
        let lookup: Arc<dyn HostLookup> = Arc::new(SystemLookup);
        Self {
            http: edge_http(
                Arc::clone(&lookup),
                Arc::new(a2a_peer),
                delegation_timeouts(DELEGATION_READ_TIMEOUT),
            ),
            http_private_network: edge_http(
                lookup,
                a2a_peer_with_bypass(true),
                delegation_timeouts(DELEGATION_READ_TIMEOUT),
            ),
            limits: A2aLimits::default(),
        }
    }

    /// [`for_delegation`](Self::for_delegation) with the silence budget a test
    /// can wait for. The production one is minutes; nothing in CI should be.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_delegation_with_read_timeout(read: Duration) -> Self {
        let lookup: Arc<dyn HostLookup> = Arc::new(SystemLookup);
        Self {
            http: edge_http(
                Arc::clone(&lookup),
                Arc::new(a2a_peer),
                delegation_timeouts(read),
            ),
            http_private_network: edge_http(
                lookup,
                a2a_peer_with_bypass(true),
                delegation_timeouts(read),
            ),
            limits: A2aLimits {
                idle_timeout: read,
                ..A2aLimits::default()
            },
        }
    }

    /// A delegation client resolving names through `lookup`, a stub in
    /// tests, under the production [`a2a_peer`] policy — and, for an agent
    /// with the opt-in, [`a2a_peer_with_bypass`]`(true)` over the same
    /// stubbed `lookup`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_lookup(lookup: Arc<dyn HostLookup>) -> Self {
        Self {
            http: edge_http(
                Arc::clone(&lookup),
                Arc::new(a2a_peer),
                delegation_timeouts(DELEGATION_READ_TIMEOUT),
            ),
            http_private_network: edge_http(
                lookup,
                a2a_peer_with_bypass(true),
                delegation_timeouts(DELEGATION_READ_TIMEOUT),
            ),
            limits: A2aLimits::default(),
        }
    }

    /// This client with other bounds on what a peer may send.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_limits(mut self, limits: A2aLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Which of the two clients to use for `config`: the default, or —
    /// only when its AGE-806 opt-in is set — the one whose resolver admits
    /// private ranges for the addresses this agent's name resolves to.
    fn http_for(&self, config: &A2aAgentConfig) -> &reqwest::Client {
        if config.allow_private_network {
            &self.http_private_network
        } else {
            &self.http
        }
    }

    /// Fetch the agent card from `<base_url>/.well-known/agent.json`.
    ///
    /// Returns `None` when the endpoint is unreachable or returns unexpected JSON.
    pub async fn fetch_agent_card(&self, config: &A2aAgentConfig) -> Result<AgentCard> {
        ensure_callable(&config.url)?;
        // Strip trailing slash and append the well-known path.
        let base = config.url.trim_end_matches('/');
        let card_url = format!("{}/.well-known/agent.json", base);

        debug!(url = %card_url, "Fetching A2A agent card");

        let req = authorized(self.http_for(config).get(&card_url), config);
        let resp = send_checked(req, &card_url, "agent card request").await?;
        let body = read_json_capped(resp, self.limits.max_event_bytes, "agent card").await?;

        let name = body
            .get("name")
            .or_else(|| body.get("displayName"))
            .and_then(|v| v.as_str())
            .unwrap_or(&config.name)
            .to_string();

        let description = body
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let skills: Vec<String> = body
            .get("skills")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        s.get("name")
                            .or(Some(s))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let supports_streaming = body
            .pointer("/capabilities/streaming")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        info!(
            url = %card_url,
            agent = %name,
            skill_count = skills.len(),
            streaming = supports_streaming,
            "A2A agent card fetched successfully"
        );

        Ok(AgentCard {
            name,
            description,
            skills,
            supports_streaming,
        })
    }

    /// Send a `message/send` JSON-RPC request to the remote A2A agent.
    ///
    /// Returns the plain-text response extracted from the task artifacts.
    pub async fn send_message(&self, config: &A2aAgentConfig, prompt: &str) -> Result<String> {
        ensure_callable(&config.url)?;
        let url = config.url.trim_end_matches('/').to_string();

        let task_id = uuid::Uuid::new_v4().to_string();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "parts": [{ "type": "text", "text": prompt }]
                },
                "taskId": task_id
            }
        });

        debug!(url = %url, agent = %config.name, "Sending A2A message/send");

        let req = authorized(self.http_for(config).post(&url).json(&body), config);
        let resp = send_checked(req, &url, "message/send").await?;
        let value =
            read_json_capped(resp, self.limits.max_event_bytes, "message/send response").await?;

        // Check for JSON-RPC error
        if let Some(err) = value.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            bail!("A2A agent returned error: {}", msg);
        }

        let text = task_outcome(&value)?;
        info!(agent = %config.name, "A2A message/send completed");
        Ok(text)
    }

    /// Send a `message/stream` JSON-RPC request and return an SSE event stream.
    ///
    /// The returned stream yields [`A2aStreamEvent`] items as the remote agent
    /// processes the request.  The stream ends after the final event (a
    /// `TaskStatusUpdateEvent` with `final: true`).
    ///
    /// Falls back to [`send_message`](Self::send_message) wrapped in a
    /// single-item stream if the remote agent does not support streaming.
    pub async fn send_message_stream(
        &self,
        config: &A2aAgentConfig,
        prompt: &str,
    ) -> Result<BoxStream<'static, Result<A2aStreamEvent>>> {
        use reqwest::header;

        ensure_callable(&config.url)?;
        let url = config.url.trim_end_matches('/').to_string();

        let task_id = uuid::Uuid::new_v4().to_string();
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/stream",
            "params": {
                "message": {
                    "parts": [{ "type": "text", "text": prompt }]
                },
                "taskId": task_id
            }
        });

        debug!(url = %url, agent = %config.name, "Sending A2A message/stream");

        let req = authorized(self.http_for(config).post(&url).json(&body), config);
        let resp = send_checked(req, &url, "message/stream").await?;

        // Check Content-Type — if not SSE, the server likely doesn't support
        // streaming and returned a normal JSON-RPC response.
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !content_type.contains("text/event-stream") {
            // Treat as a regular JSON response (same as message/send).
            let value = read_json_capped(
                resp,
                self.limits.max_event_bytes,
                "non-streaming message/stream response",
            )
            .await?;

            let text = value
                .pointer("/result/artifacts/0/parts/0/text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let tid = value
                .pointer("/result/id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let stream = futures::stream::iter(vec![
                Ok(A2aStreamEvent::StatusUpdate {
                    task_id: tid.clone(),
                    state: "completed".to_string(),
                    is_final: true,
                    message: None,
                    metadata: None,
                }),
                Ok(A2aStreamEvent::ArtifactUpdate {
                    task_id: tid,
                    text,
                    last_chunk: true,
                }),
            ]);

            return Ok(Box::pin(stream));
        }

        // Parse the SSE byte stream into A2aStreamEvent items, within the
        // limits: an event, the whole stream and any silence are bounded.
        let byte_stream = resp.bytes_stream();
        let limits = self.limits;
        let event_stream = async_stream::stream! {
            use futures::StreamExt;

            let mut buffer = String::new();
            let mut stream = byte_stream;
            let mut total = 0usize;

            loop {
                let result = match tokio::time::timeout(limits.idle_timeout, stream.next()).await {
                    Ok(Some(result)) => result,
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(anyhow::anyhow!(
                            "A2A stream from {url} sent nothing for {:?}; giving up on it",
                            limits.idle_timeout
                        ));
                        return;
                    }
                };
                let bytes = match result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(anyhow::anyhow!("SSE stream error: {}", e));
                        return;
                    }
                };
                total += bytes.len();
                if total > limits.max_stream_bytes {
                    yield Err(anyhow::anyhow!(
                        "A2A stream from {url} exceeds the {}-byte stream cap; refusing it",
                        limits.max_stream_bytes
                    ));
                    return;
                }
                buffer.push_str(&String::from_utf8_lossy(&bytes).replace("\r\n", "\n"));

                // SSE events are separated by double newlines.
                while let Some(pos) = buffer.find("\n\n") {
                    if pos > limits.max_event_bytes {
                        yield Err(event_over_cap(&url, limits.max_event_bytes));
                        return;
                    }
                    let event_block = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    if let Some(evt) = parse_sse_event(&event_block) {
                        let is_final = matches!(&evt, A2aStreamEvent::StatusUpdate { is_final: true, .. });
                        yield Ok(evt);
                        if is_final {
                            return;
                        }
                    }
                }

                // An unfinished event already over the cap will not shrink.
                if buffer.len() > limits.max_event_bytes {
                    yield Err(event_over_cap(&url, limits.max_event_bytes));
                    return;
                }
            }

            // Process any remaining data in the buffer.
            if !buffer.trim().is_empty() && let Some(evt) = parse_sse_event(&buffer) {
                yield Ok(evt);
            }
        };

        Ok(Box::pin(event_stream))
    }
}

impl A2aClient {
    /// Answer a task parked in `input-required`.
    ///
    /// A2A resumes a task with `message/send` carrying the task's id on the
    /// message; the answers ride in the message's `metadata` under
    /// [`CLARIFICATION_METADATA_KEY`], and a text part spells them out for
    /// a reader that only reads text. The task's stream, which the caller
    /// is still consuming, is where the task's progress continues.
    pub async fn send_task_input(
        &self,
        config: &A2aAgentConfig,
        task_id: &str,
        request_id: &str,
        answers: &[ClarificationAnswer],
    ) -> Result<()> {
        ensure_callable(&config.url)?;
        let url = config.url.trim_end_matches('/').to_string();
        let text = answers
            .iter()
            .map(|a| format!("{}: {}", a.id, a.answer))
            .collect::<Vec<_>>()
            .join("\n");
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "taskId": task_id,
                    "parts": [{ "type": "text", "text": text }],
                    "metadata": {
                        CLARIFICATION_METADATA_KEY: {
                            "requestId": request_id,
                            "answers": answers,
                        }
                    }
                }
            }
        });

        debug!(url = %url, agent = %config.name, task = %task_id, "Answering a parked A2A task");

        let req = authorized(self.http_for(config).post(&url).json(&body), config);
        let resp = send_checked(req, &url, "message/send").await?;
        let value =
            read_json_capped(resp, self.limits.max_event_bytes, "message/send response").await?;
        if let Some(err) = value.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            bail!("A2A agent refused the answer: {}", msg);
        }
        Ok(())
    }
}

fn event_over_cap(url: &str, max: usize) -> anyhow::Error {
    anyhow::anyhow!("an A2A stream event from {url} exceeds the {max}-byte event cap; refusing it")
}

impl Default for A2aClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SSE parsing helper
// ---------------------------------------------------------------------------

/// Parse a single SSE event block into an [`A2aStreamEvent`].
///
/// An SSE event block looks like:
/// ```text
/// data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-123","status":{"state":"working"},"final":false}}
/// ```
fn parse_sse_event(block: &str) -> Option<A2aStreamEvent> {
    // Extract the `data:` line(s).
    let data: String = block
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .or_else(|| line.strip_prefix("data: "))
        })
        .collect::<Vec<_>>()
        .join("\n");

    if data.is_empty() {
        return None;
    }

    let json: Value = serde_json::from_str(&data).ok()?;

    let result = json.get("result")?;
    let task_id = result
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let is_final = result
        .get("final")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Artifact update?
    if let Some(artifact) = result.get("artifact") {
        let text = artifact
            .pointer("/parts/0/text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let last_chunk = artifact
            .get("lastChunk")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        return Some(A2aStreamEvent::ArtifactUpdate {
            task_id,
            text,
            last_chunk,
        });
    }

    // Status update?
    if let Some(status) = result.get("status") {
        let state = status
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let message = status
            .pointer("/message/parts/0/text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let metadata = status.get("metadata").cloned();
        return Some(A2aStreamEvent::StatusUpdate {
            task_id,
            state,
            is_final,
            message,
            metadata,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sse_status_update() {
        let block = r#"data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-abc","status":{"state":"working"},"final":false}}"#;
        let evt = parse_sse_event(block).unwrap();
        match evt {
            A2aStreamEvent::StatusUpdate {
                task_id,
                state,
                is_final,
                ..
            } => {
                assert_eq!(task_id, "task-abc");
                assert_eq!(state, "working");
                assert!(!is_final);
            }
            _ => panic!("Expected StatusUpdate"),
        }
    }

    #[test]
    fn parse_sse_input_required_carries_the_request() {
        let block = r#"data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-abc","status":{"state":"input-required","message":{"parts":[{"type":"text","text":"Which database?"}]},"metadata":{"clarification":{"id":"req-1","questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}},"final":false}}"#;
        let evt = parse_sse_event(block).unwrap();
        let A2aStreamEvent::StatusUpdate {
            state,
            message,
            metadata,
            ..
        } = evt
        else {
            panic!("Expected StatusUpdate");
        };
        assert_eq!(state, "input-required");
        assert_eq!(message.as_deref(), Some("Which database?"));
        let request = A2aClarificationRequest::from_status_metadata(metadata.as_ref())
            .expect("the request is in the metadata");
        assert_eq!(request.id, "req-1");
        assert_eq!(request.questions[0].options, vec!["Postgres", "SQLite"]);
    }

    #[test]
    fn a_status_without_a_request_cannot_be_answered() {
        assert!(A2aClarificationRequest::from_status_metadata(None).is_none());
        let usage_only = json!({ "usage": { "inputTokens": 1 } });
        assert!(A2aClarificationRequest::from_status_metadata(Some(&usage_only)).is_none());
    }

    /// AGE-415 / AGE-682: the terminal status's `metadata.usage`, spelled
    /// as the broker's worker mapper writes it, reads back as one
    /// `TokenUsage` per line, each naming its model.
    #[test]
    fn a_terminal_status_carries_the_workers_usage() {
        let block = r#"data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-abc","status":{"state":"completed","metadata":{"usage":{"inputTokens":120,"outputTokens":34,"cacheReadTokens":900,"cacheWriteTokens":50,"lines":[{"model":{"provider":"ollama","model_id":"qwen3:32b"},"inputTokens":120,"outputTokens":34,"cacheReadTokens":900,"cacheWriteTokens":50,"at":1790000000000,"durationMs":1500}]}}},"final":true}}"#;
        let A2aStreamEvent::StatusUpdate { metadata, .. } = parse_sse_event(block).unwrap() else {
            panic!("Expected StatusUpdate");
        };
        let lines = usage_from_status_metadata(metadata.as_ref());
        assert_eq!(lines.len(), 1, "usage is in the metadata");
        let usage = &lines[0];
        assert_eq!(usage.input_tokens, 120);
        assert_eq!(usage.output_tokens, 34);
        assert_eq!(usage.cache_read_tokens, 900);
        assert_eq!(usage.cache_write_tokens, 50);
        assert_eq!(
            usage.model,
            Some(ModelRef {
                provider: crate::settings::models::providers_store::ProviderType::Ollama,
                model_id: "qwen3:32b".to_string(),
            })
        );
        assert_eq!(
            usage.at,
            Some(UNIX_EPOCH + Duration::from_millis(1_790_000_000_000))
        );
        assert_eq!(usage.duration_ms, 1500);
        assert_eq!(usage.delegated_to, None, "the caller names the agent");

        assert!(usage_from_status_metadata(None).is_empty());
        let clarification_only = json!({ "clarification": { "id": "r", "questions": [] } });
        assert!(usage_from_status_metadata(Some(&clarification_only)).is_empty());
    }

    /// What the writer writes, the reader reads — model, tokens and time,
    /// line for line.
    #[test]
    fn usage_lines_round_trip_through_the_metadata() {
        let line = TokenUsage {
            model: Some(ModelRef {
                provider: crate::settings::models::providers_store::ProviderType::OpenRouter,
                model_id: "anthropic/claude-sonnet-5".to_string(),
            }),
            at: Some(UNIX_EPOCH + Duration::from_millis(1_790_000_000_123)),
            duration_ms: 42,
            cache_read_tokens: 7,
            cache_write_tokens: 3,
            ..TokenUsage::new(100, 20)
        };
        let metadata = json!({ "usage": wire_usage(std::slice::from_ref(&line)) });
        let back = usage_from_status_metadata(Some(&metadata));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].model, line.model);
        assert_eq!(back[0].at, line.at);
        assert_eq!(back[0].duration_ms, 42);
        assert_eq!(
            (
                back[0].input_tokens,
                back[0].output_tokens,
                back[0].cache_read_tokens,
                back[0].cache_write_tokens
            ),
            (100, 20, 7, 3)
        );
    }

    /// AGE-682: a line that does not parse costs only itself, not the rest
    /// of the worker's report.
    #[test]
    fn a_bad_usage_line_is_skipped_not_the_report() {
        let metadata = json!({ "usage": { "lines": [
            { "inputTokens": 10, "outputTokens": 2 },
            { "model": { "provider": "no_such_provider", "model_id": "x" }, "inputTokens": 5 },
            { "inputTokens": 7 },
        ] } });
        let lines = usage_from_status_metadata(Some(&metadata));
        let inputs: Vec<u32> = lines.iter().map(|l| l.input_tokens).collect();
        assert_eq!(inputs, vec![10, 7]);
    }

    /// AGE-682: a report with the four totals but no `lines` still counts,
    /// as one line naming no model; a report with neither is no usage.
    #[test]
    fn totals_without_lines_are_one_unattributed_line() {
        let metadata = json!({ "usage": { "inputTokens": 30, "outputTokens": 15 } });
        let lines = usage_from_status_metadata(Some(&metadata));
        assert_eq!(lines.len(), 1);
        assert_eq!((lines[0].input_tokens, lines[0].output_tokens), (30, 15));
        assert_eq!(lines[0].model, None);

        assert!(usage_from_status_metadata(Some(&json!({ "usage": {} }))).is_empty());
    }

    /// AGE-467: a worker's compacted trace rides the terminal status next to
    /// its usage, under its own key, and only when the metadata carries one.
    #[test]
    fn a_terminal_status_carries_the_workers_trace() {
        let trace = "### read_file (ok)\ninput: {}\noutput: # Chatty";
        let metadata = json!({ "trace": trace });
        assert_eq!(
            trace_from_status_metadata(Some(&metadata)).as_deref(),
            Some(trace)
        );

        assert!(trace_from_status_metadata(None).is_none());
        let usage_only = json!({ "usage": { "inputTokens": 1 } });
        assert!(trace_from_status_metadata(Some(&usage_only)).is_none());
    }

    /// The two keys ride the same metadata object without disturbing each
    /// other — what the mapper's terminal status actually sends when a
    /// parent asked for both (AGE-467).
    #[test]
    fn usage_and_trace_round_trip_from_the_same_metadata() {
        let trace = "### read_file (ok)\ninput: {}\noutput: # Chatty";
        let metadata = json!({
            "usage": wire_usage(&[TokenUsage::new(120, 34)]),
            "trace": trace,
        });

        let usage = usage_from_status_metadata(Some(&metadata));
        assert_eq!(usage.len(), 1, "usage is in the metadata");
        assert_eq!((usage[0].input_tokens, usage[0].output_tokens), (120, 34));
        assert_eq!(
            trace_from_status_metadata(Some(&metadata)).as_deref(),
            Some(trace),
            "the trace reads back independently of the usage key"
        );
    }

    /// RC-0 (AGE-649): a captured conversation rides the terminal status
    /// under its own key, independent of usage and trace.
    #[test]
    fn a_terminal_status_carries_the_workers_conversation() {
        let metadata = json!({ "conversation": [{"role": "user", "content": "hi"}] });
        let conversation = conversation_from_status_metadata(Some(&metadata))
            .expect("the conversation is in the metadata");
        assert_eq!(conversation[0]["content"], "hi");

        assert!(conversation_from_status_metadata(None).is_none());
        let usage_only = json!({ "usage": { "inputTokens": 1 } });
        assert!(conversation_from_status_metadata(Some(&usage_only)).is_none());
    }

    /// An oversized conversation reports its byte count instead, under a
    /// different key, so the two never collide.
    #[test]
    fn a_too_large_conversation_reports_its_byte_count_instead() {
        let metadata = json!({ "conversationTooLarge": 33_554_433u64 });
        assert!(conversation_from_status_metadata(Some(&metadata)).is_none());
        assert_eq!(
            conversation_too_large_from_status_metadata(Some(&metadata)),
            Some(33_554_433)
        );
        assert!(conversation_too_large_from_status_metadata(None).is_none());
    }

    #[test]
    fn parse_sse_artifact_update() {
        let block = r#"data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-abc","artifact":{"parts":[{"type":"text","text":"Hello world"}],"index":0,"lastChunk":true}}}"#;
        let evt = parse_sse_event(block).unwrap();
        match evt {
            A2aStreamEvent::ArtifactUpdate {
                task_id,
                text,
                last_chunk,
            } => {
                assert_eq!(task_id, "task-abc");
                assert_eq!(text, "Hello world");
                assert!(last_chunk);
            }
            _ => panic!("Expected ArtifactUpdate"),
        }
    }

    #[test]
    fn parse_sse_completed_final() {
        let block = r#"data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-123","status":{"state":"completed"},"final":true}}"#;
        let evt = parse_sse_event(block).unwrap();
        match evt {
            A2aStreamEvent::StatusUpdate {
                state, is_final, ..
            } => {
                assert_eq!(state, "completed");
                assert!(is_final);
            }
            _ => panic!("Expected StatusUpdate"),
        }
    }

    #[test]
    fn parse_sse_empty_block_returns_none() {
        assert!(parse_sse_event("").is_none());
        assert!(parse_sse_event("event: keep-alive").is_none());
    }

    #[test]
    fn parse_sse_with_space_after_colon() {
        let block = r#"data:{"jsonrpc":"2.0","id":1,"result":{"id":"t","status":{"state":"working"},"final":false}}"#;
        let evt = parse_sse_event(block);
        assert!(evt.is_some());
    }

    // ── AGE-319: what the delegation transport may and may not bound ────────

    /// An SSE server that sends `frames` `gap` apart, then the terminal one.
    ///
    /// Stands in for a broker carrying a delegated turn: the frames are its
    /// keep-alives and progress, the gap is how quiet it goes between them.
    async fn sse_server(frames: usize, gap: Duration) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("the bound address").port();

        let server = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let _ = socket.read(&mut buffer).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
                )
                .await;

            // Each frame goes out first and the gap follows it, so a caller
            // always sees the stream alive before it can see it go quiet.
            for _ in 0..frames {
                let frame = "data:{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"id\":\"t\",\
                             \"status\":{\"state\":\"working\"},\"final\":false}}\n\n";
                let chunked = format!("{:x}\r\n{}\r\n", frame.len(), frame);
                if socket.write_all(chunked.as_bytes()).await.is_err() {
                    return;
                }
                tokio::time::sleep(gap).await;
            }

            let last = "data:{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"id\":\"t\",\
                        \"status\":{\"state\":\"completed\"},\"final\":true}}\n\n";
            let chunked = format!("{:x}\r\n{}\r\n0\r\n\r\n", last.len(), last);
            let _ = socket.write_all(chunked.as_bytes()).await;
            let _ = socket.flush().await;
        });

        (format!("http://127.0.0.1:{port}"), server)
    }

    fn agent(url: &str) -> A2aAgentConfig {
        A2aAgentConfig {
            name: "worker".to_string(),
            url: url.to_string(),
            api_key: None,
            enabled: true,
            skills: vec![],
            allow_private_network: false,
        }
    }

    /// Drive a delegation to its end, reporting how many events arrived and
    /// how it finished. The count matters: a test that expects a *silence*
    /// failure has to know the stream was alive first, or a refused
    /// connection would satisfy it just as well.
    async fn drive(client: &A2aClient, url: &str) -> (usize, Result<()>) {
        use futures::StreamExt;

        let mut stream = match client
            .send_message_stream(&agent(url), "do the thing")
            .await
        {
            Ok(stream) => stream,
            Err(error) => return (0, Err(error)),
        };
        let mut seen = 0usize;
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                return (seen, Err(error));
            }
            seen += 1;
        }
        (seen, Ok(()))
    }

    /// A delegation may take longer than the silence budget and still finish:
    /// the transport bounds gaps, not duration. Before AGE-319 this was a
    /// total timeout, so a turn that outlived it died mid-answer however
    /// chatty the far side was.
    #[tokio::test]
    async fn a_stream_may_run_far_longer_than_its_silence_budget() {
        let gap = Duration::from_millis(40);
        let (url, _server) = sse_server(12, gap).await;
        // Runs ~480 ms in 40 ms steps, against a 200 ms budget for any one
        // step: more than twice as long as a total timeout of that size.
        let client = A2aClient::for_delegation_with_read_timeout(Duration::from_millis(200));

        let (seen, outcome) = drive(&client, &url).await;
        outcome.expect("a long but talkative delegation completes");
        assert_eq!(seen, 13, "every frame arrives, including the terminal one");
    }

    /// Silence is still a failure — that is what the read timeout is for, and
    /// it is why dropping the total timeout does not mean waiting forever on a
    /// socket nobody is on the other end of.
    #[tokio::test]
    async fn a_stream_that_goes_quiet_fails() {
        // One frame, then a gap five times the budget.
        let (url, _server) = sse_server(1, Duration::from_millis(400)).await;
        let client = A2aClient::for_delegation_with_read_timeout(Duration::from_millis(80));

        let (seen, outcome) = drive(&client, &url).await;
        assert!(
            seen > 0,
            "the stream has to be alive before it goes quiet, or this proves nothing"
        );
        assert!(
            outcome.is_err(),
            "a stream silent past the read timeout must fail"
        );
    }

    /// The ordering AGE-319 is about, checked where a reader will see it. The
    /// compile-time assertion in this module is what enforces it.
    #[test]
    fn the_transport_outlasts_the_question_it_carries() {
        assert!(
            DELEGATION_READ_TIMEOUT > CLARIFICATION_TIMEOUT,
            "a question nobody answers must fail as an unanswered question, \
             not as a dead socket"
        );
    }

    #[test]
    fn a_failed_task_is_an_error_that_carries_its_reason() {
        let reply = json!({
            "result": {
                "id": "task-1",
                "status": {
                    "state": "failed",
                    "message": { "parts": [{ "type": "text", "text": "the worker asked: Which database?" }] },
                },
                "artifacts": [{ "parts": [{ "type": "text", "text": "" }] }],
            }
        });

        let error = task_outcome(&reply).expect_err("a failed task is not a success");
        let text = format!("{error:#}");
        assert!(text.contains("Which database?"), "{text}");
        assert!(text.contains("failed"), "{text}");
    }

    #[test]
    fn a_failed_task_without_a_reason_still_fails() {
        let reply = json!({ "result": { "status": { "state": "failed" } } });
        let error = task_outcome(&reply).expect_err("still an error");
        assert!(format!("{error:#}").contains("no reason"));
    }

    #[test]
    fn a_completed_task_returns_its_text() {
        let reply = json!({
            "result": {
                "status": { "state": "completed" },
                "artifacts": [{ "parts": [{ "type": "text", "text": "42" }] }],
            }
        });
        assert_eq!(task_outcome(&reply).unwrap(), "42");
    }

    /// An agent that says nothing about state is taken at its word, as before:
    /// this reads a failure, it does not invent one.
    #[test]
    fn a_reply_without_a_state_is_still_its_text() {
        let reply = json!({
            "result": { "artifacts": [{ "parts": [{ "type": "text", "text": "hello" }] }] }
        });
        assert_eq!(task_outcome(&reply).unwrap(), "hello");
    }

    #[test]
    fn agent_card_supports_streaming_field() {
        let card = AgentCard {
            name: "test".to_string(),
            description: "test agent".to_string(),
            skills: vec![],
            supports_streaming: true,
        };
        assert!(card.supports_streaming);
    }

    /// SEC-16 / AGE-756: a remote A2A agent configured with a plain
    /// `http://` URL to a real host must be refused before any request
    /// leaves — never dialed, so a prompt, an answer, or the agent's bearer
    /// token never has the chance to cross the wire in clear text.
    #[tokio::test]
    async fn plain_http_remote_agent_is_refused() {
        let client = A2aClient::new();
        let remote = agent("http://voucher-agent.example.com/a2a");

        let err = client
            .fetch_agent_card(&remote)
            .await
            .expect_err("a plain-http remote agent card fetch must be refused");
        assert!(format!("{err:#}").contains("https"), "{err:#}");

        let err = client
            .send_message(&remote, "hi")
            .await
            .expect_err("a plain-http remote agent send must be refused");
        assert!(format!("{err:#}").contains("https"), "{err:#}");

        let err = client
            .send_task_input(&remote, "task-1", "req-1", &[])
            .await
            .expect_err("a plain-http remote agent answer must be refused");
        assert!(format!("{err:#}").contains("https"), "{err:#}");
    }

    /// A loopback A2A agent — e.g. a locally running dev worker — keeps
    /// working over `http://`; only a real remote host requires TLS.
    #[tokio::test]
    async fn loopback_http_is_allowed() {
        let client = A2aClient::new();
        let local = agent("http://127.0.0.1:1/a2a");

        // The gate itself must not be what refuses this: it fails for a
        // mundane reason (nothing listens on this port), not as insecure.
        let err = client
            .send_message(&local, "hi")
            .await
            .expect_err("nothing listens on this port");
        assert!(
            !format!("{err:#}").contains("plain http"),
            "a loopback agent must not be rejected as insecure: {err:#}"
        );
    }

    // ── AGE-767: the A2A edge (ADR-0021 step 0) ────────────────────────────

    use crate::services::ssrf_guard::HostLookup;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::net::IpAddr;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A resolver stub: answers from a script, in order, and counts lookups.
    struct Scripted {
        answers: Mutex<VecDeque<std::io::Result<Vec<IpAddr>>>>,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn new(answers: Vec<std::io::Result<Vec<IpAddr>>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl HostLookup for Scripted {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(std::io::Error::other("stub: no scripted answer left")));
            Box::pin(async move { answer })
        }
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// Read one HTTP request off `socket`: the headers, then as much body
    /// as `content-length` says.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if raw.len() >= end + 4 + length {
                    return String::from_utf8_lossy(&raw).into_owned();
                }
            }
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => return String::from_utf8_lossy(&raw).into_owned(),
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// A loopback HTTP server that answers each connection with the next
    /// canned response, closing it after, and hands back what it was sent.
    async fn http_server(responses: Vec<String>) -> (u16, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                seen.push(read_request(&mut socket).await);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
            seen
        });
        (port, server)
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const COMPLETED: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"status":{"state":"completed"},"artifacts":[{"parts":[{"text":"done"}]}]}}"#;

    /// Nothing may have connected to `listener`: a connection, had one been
    /// made, would be waiting in its backlog.
    async fn assert_never_connected(listener: &tokio::net::TcpListener, why: &str) {
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "{why}");
    }

    /// The stub first answers the checked address, then — a rebinding name —
    /// a private one. Each connection resolves exactly once and goes to what
    /// that lookup returned; the rebound answer is refused, never dialed.
    ///
    /// The TLS rule (AGE-756) only lets a plain-http test server sit behind
    /// `localhost`, whose admitted class is loopback, so loopback plays the
    /// checked address and `10.0.0.5` the private one it is rebound to. A
    /// public name rebound to loopback is refused the same way, before any
    /// connection (the second half), under the production policy.
    #[tokio::test]
    async fn a2a_refuses_private_address_after_rebind() {
        let (port, server) = http_server(vec![json_response(COMPLETED)]).await;
        let lookup = Scripted::new(vec![Ok(vec![ip("127.0.0.1")]), Ok(vec![ip("10.0.0.5")])]);
        let client = A2aClient::with_lookup(lookup.clone());
        let local = agent(&format!("http://localhost:{port}/a2a"));

        let text = client
            .send_message(&local, "hi")
            .await
            .expect("the checked address is the one connected to");
        assert_eq!(text, "done");
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 1, "resolved once");

        let err = client
            .send_message(&local, "hi")
            .await
            .expect_err("a name rebound to a private address must be refused");
        assert!(format!("{err:#}").contains("SSRF"), "{err:#}");
        assert_eq!(lookup.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            server.await.unwrap().len(),
            1,
            "only the checked address was reached"
        );

        // A public name answering loopback: refused at resolution, so the
        // loopback server listening there is never connected to.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let lookup = Scripted::new(vec![Ok(vec![ip("127.0.0.1")])]);
        let client = A2aClient::with_lookup(lookup);
        let err = client
            .send_message(
                &agent(&format!("https://agent.example.com:{port}/a2a")),
                "hi",
            )
            .await
            .expect_err("a public name resolving to loopback must be refused");
        assert!(format!("{err:#}").contains("SSRF"), "{err:#}");
        assert_never_connected(&listener, "a refused address must never be dialed").await;
    }

    // ── AGE-806: per-agent opt-in for private-network agents ───────────────

    /// Without the opt-in (the default, `allow_private_network: false`), a
    /// hostname that resolves to a private address is refused — unchanged
    /// from before the flag existed.
    #[tokio::test]
    async fn a2a_private_address_refused_without_opt_in() {
        let lookup = Scripted::new(vec![Ok(vec![ip("10.0.0.5")])]);
        let client = A2aClient::with_lookup(lookup);
        let remote = agent("https://agent.lan.example/a2a");
        assert!(!remote.allow_private_network);

        let err = client
            .send_message(&remote, "hi")
            .await
            .expect_err("a name resolving to a private address must be refused without the opt-in");
        assert!(format!("{err:#}").contains("SSRF"), "{err:#}");
    }

    /// With the opt-in on, a hostname resolving to a LAN or Tailscale (CGN)
    /// address is admitted by the resolver: the attempt then fails for an
    /// ordinary reason — nothing listens on this port — never as SSRF. Same
    /// technique as `loopback_http_is_allowed`: the gate itself must not be
    /// what refuses this. The exact ranges (RFC-1918, `100.64.0.0/10`, ULA)
    /// are covered address-by-address in `ssrf_guard`'s
    /// `a2a_peer_with_bypass_on_admits_lan_and_tailscale_names`; this proves
    /// the A2A client actually wires the per-agent flag through to the
    /// resolver it dials with.
    #[tokio::test]
    async fn a2a_lan_and_tailscale_admitted_with_opt_in() {
        let lookup = Scripted::new(vec![Ok(vec![ip("127.0.0.1")])]);
        let client = A2aClient::with_lookup(lookup);
        let mut remote = agent("https://agent.lan.example:1/a2a");
        remote.allow_private_network = true;

        let err = client
            .send_message(&remote, "hi")
            .await
            .expect_err("nothing listens on this port");
        assert!(
            !format!("{err:#}").contains("SSRF"),
            "the opt-in must admit a resolved private address: {err:#}"
        );
    }

    /// Cloud metadata (`169.254.0.0/16`) stays refused even with the opt-in
    /// on — that carve-out is applied before the flag is ever consulted.
    #[tokio::test]
    async fn a2a_opt_in_still_refuses_link_local_metadata() {
        let lookup = Scripted::new(vec![Ok(vec![ip("169.254.169.254")])]);
        let client = A2aClient::with_lookup(lookup);
        let mut remote = agent("https://agent.lan.example/a2a");
        remote.allow_private_network = true;

        let err = client
            .send_message(&remote, "hi")
            .await
            .expect_err("cloud metadata must stay refused even with the opt-in");
        assert!(format!("{err:#}").contains("SSRF"), "{err:#}");
    }

    /// The opt-in lives on the agent config, not the client or a global
    /// setting: one client, one resolved address, and only one of the two
    /// agents has the flag set.
    #[tokio::test]
    async fn a2a_opt_in_is_per_agent() {
        let lookup = Scripted::new(vec![
            Ok(vec![ip("127.0.0.1")]), // agent-a, opted in
            Ok(vec![ip("127.0.0.1")]), // agent-b, not opted in
        ]);
        let client = A2aClient::with_lookup(lookup);

        let mut agent_a = agent("https://agent-a.lan.example:1/a2a");
        agent_a.allow_private_network = true;
        let err_a = client
            .send_message(&agent_a, "hi")
            .await
            .expect_err("nothing listens on this port");
        assert!(
            !format!("{err_a:#}").contains("SSRF"),
            "agent-a's own opt-in must admit it: {err_a:#}"
        );

        let mut agent_b = agent("https://agent-b.lan.example:1/a2a");
        agent_b.allow_private_network = false;
        let err_b = client
            .send_message(&agent_b, "hi")
            .await
            .expect_err("agent-b has no opt-in, so the same address must be refused");
        assert!(
            format!("{err_b:#}").contains("SSRF"),
            "agent-a's opt-in must not leak to agent-b: {err_b:#}"
        );
    }

    #[tokio::test]
    async fn a2a_resolution_error_refuses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let local = agent(&format!("http://localhost:{port}/a2a"));
        let lookup = Scripted::new(vec![
            Err(std::io::Error::other("stub: resolver down")),
            Err(std::io::Error::other("stub: resolver down")),
            Ok(vec![]),
        ]);
        let client = A2aClient::with_lookup(lookup);

        let err = client
            .fetch_agent_card(&local)
            .await
            .expect_err("card must be refused");
        assert!(
            format!("{err:#}").contains("could not be resolved"),
            "{err:#}"
        );
        let err = match client.send_message_stream(&local, "hi").await {
            Ok(_) => panic!("a stream must be refused"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("could not be resolved"),
            "{err:#}"
        );
        let err = client
            .send_message(&local, "hi")
            .await
            .expect_err("an empty answer too");
        assert!(format!("{err:#}").contains("no address"), "{err:#}");

        assert_never_connected(&listener, "an unresolved name must never be dialed").await;
    }

    #[tokio::test]
    async fn a2a_does_not_follow_redirect() {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let elsewhere = format!(
            "http://127.0.0.1:{}/steal",
            target.local_addr().unwrap().port()
        );
        let redirect = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: {elsewhere}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let (port, server) = http_server(vec![redirect.clone(), redirect.clone(), redirect]).await;
        let mut keyed = agent(&format!("http://127.0.0.1:{port}/a2a"));
        keyed.api_key = Some("agent-key".to_string());
        let client = A2aClient::new();

        let err = client
            .send_message(&keyed, "hi")
            .await
            .expect_err("a redirect is an error");
        assert!(
            format!("{err:#}").contains("do not follow redirects"),
            "{err:#}"
        );
        let err = match client.send_message_stream(&keyed, "hi").await {
            Ok(_) => panic!("a redirected stream must fail"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("do not follow redirects"),
            "{err:#}"
        );
        let err = client
            .fetch_agent_card(&keyed)
            .await
            .expect_err("a redirected card too");
        assert!(
            format!("{err:#}").contains("do not follow redirects"),
            "{err:#}"
        );

        assert_eq!(server.await.unwrap().len(), 3);
        assert_never_connected(&target, "the redirect target must never be requested").await;
    }

    /// An SSE server that writes `chunks` as they are, then holds the
    /// connection open for `hold`.
    async fn raw_sse_server(chunks: Vec<String>, hold: Duration) -> String {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            read_request(&mut socket).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                .await;
            for chunk in chunks {
                let framed = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                if socket.write_all(framed.as_bytes()).await.is_err() {
                    return;
                }
            }
            let _ = socket.flush().await;
            tokio::time::sleep(hold).await;
        });
        format!("http://127.0.0.1:{port}")
    }

    const WORKING: &str = "data:{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"id\":\"t\",\"status\":{\"state\":\"working\"},\"final\":false}}\n\n";

    #[tokio::test]
    async fn a2a_sse_over_cap_fails_call() {
        let limits = A2aLimits {
            max_event_bytes: 1024,
            max_stream_bytes: 4096,
            idle_timeout: Duration::from_millis(300),
        };
        let client = A2aClient::new().with_limits(limits);

        // One event larger than the event cap, never terminated.
        let huge = format!("data:{}", "x".repeat(2048));
        let url = raw_sse_server(vec![huge], Duration::from_secs(5)).await;
        let (seen, outcome) = drive(&client, &url).await;
        let err = outcome.expect_err("an oversized event must fail the call");
        assert_eq!(seen, 0);
        assert!(format!("{err:#}").contains("event cap"), "{err:#}");

        // Small events that add up past the stream cap.
        let url = raw_sse_server(vec![WORKING.to_string(); 64], Duration::from_secs(5)).await;
        let (_, outcome) = drive(&client, &url).await;
        let err = outcome.expect_err("a stream past its total cap must fail the call");
        assert!(format!("{err:#}").contains("stream cap"), "{err:#}");

        // One event, then silence past the idle timeout.
        let url = raw_sse_server(vec![WORKING.to_string()], Duration::from_secs(5)).await;
        let (seen, outcome) = drive(&client, &url).await;
        let err = outcome.expect_err("a stream idle past its timeout must fail the call");
        assert_eq!(seen, 1, "the stream was alive before it went quiet");
        assert!(format!("{err:#}").contains("sent nothing"), "{err:#}");
    }

    /// Every A2A request carries the called agent's own bearer and nothing
    /// else that could be a credential: not another agent's key, not a
    /// provider key, no cookie; and an agent without a key gets none.
    #[tokio::test]
    async fn a2a_sends_only_its_own_credential() {
        let card = r#"{"name":"a","skills":[],"capabilities":{"streaming":false}}"#;
        let (port, server) = http_server(vec![
            json_response(card),
            json_response(COMPLETED),
            json_response(COMPLETED),
            json_response(COMPLETED),
            json_response(COMPLETED),
        ])
        .await;
        let url = format!("http://127.0.0.1:{port}/a2a");
        let with_key = |key: Option<&str>| A2aAgentConfig {
            api_key: key.map(str::to_string),
            ..agent(&url)
        };
        let (a, b, none) = (
            with_key(Some("key-a")),
            with_key(Some("key-b")),
            with_key(None),
        );
        let client = A2aClient::new();

        client.fetch_agent_card(&a).await.expect("card");
        client.send_message(&a, "hi").await.expect("send");
        let _ = drive(&client, &url).await; // no key: `drive` uses `agent(url)`
        client.send_message(&b, "hi").await.expect("send b");
        client.send_message(&none, "hi").await.expect("send none");
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 5);

        let allowed = [
            "host",
            "user-agent",
            "accept",
            "accept-encoding",
            "content-type",
            "content-length",
            "authorization",
        ];
        for (request, expected) in
            requests
                .iter()
                .zip([Some("key-a"), Some("key-a"), None, Some("key-b"), None])
        {
            let headers: Vec<(String, String)> = request
                .split("\r\n\r\n")
                .next()
                .unwrap()
                .lines()
                .skip(1)
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
                .collect();
            for (name, _) in &headers {
                assert!(
                    allowed.contains(&name.as_str()),
                    "unexpected header {name}: {request}"
                );
            }
            let auth: Vec<&str> = headers
                .iter()
                .filter(|(k, _)| k == "authorization")
                .map(|(_, v)| v.as_str())
                .collect();
            match expected {
                Some(key) => assert_eq!(auth, vec![format!("Bearer {key}").as_str()]),
                None => assert!(auth.is_empty(), "no key configured, none sent: {request}"),
            }
            let other = if expected == Some("key-a") {
                "key-b"
            } else {
                "key-a"
            };
            assert!(
                !request.contains(other),
                "another agent's key leaked: {request}"
            );
        }
    }
}
