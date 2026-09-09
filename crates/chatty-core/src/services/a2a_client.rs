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
use tracing::{debug, info};

use std::time::Duration;

use crate::models::clarification_store::{
    CLARIFICATION_TIMEOUT, ClarificationAnswer, ClarifyingQuestion,
};
use crate::settings::models::a2a_store::A2aAgentConfig;

/// The key under a status's `metadata` that carries what an
/// `input-required` task is waiting for, and under an answering message's
/// `metadata` that carries the answers. The broker's spelling
/// (`chatty_protocol_gateway::handlers`), repeated here because this crate
/// is below the gateway.
pub const CLARIFICATION_METADATA_KEY: &str = "clarification";

/// What a task parked in `input-required` is waiting for: an `ask_user`
/// call somewhere down the chain, with the request id its store resolves
/// on. Field for field the broker's `InputRequest`.
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

/// A lightweight HTTP client for remote A2A agents.
#[derive(Clone)]
pub struct A2aClient {
    http: reqwest::Client,
}

async fn detailed_http_error(operation: &str, resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
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
        Self {
            http: crate::services::http_client::default_client(30),
        }
    }

    /// A client for driving a delegation stream.
    ///
    /// Bounds silence rather than duration — see [`DELEGATION_READ_TIMEOUT`].
    pub fn for_delegation() -> Self {
        Self {
            http: crate::services::http_client::streaming_client(
                DELEGATION_CONNECT_TIMEOUT,
                DELEGATION_READ_TIMEOUT,
            ),
        }
    }

    /// [`for_delegation`](Self::for_delegation) with the silence budget a test
    /// can wait for. The production one is minutes; nothing in CI should be.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_delegation_with_read_timeout(read: Duration) -> Self {
        Self {
            http: crate::services::http_client::streaming_client(DELEGATION_CONNECT_TIMEOUT, read),
        }
    }

    /// Fetch the agent card from `<base_url>/.well-known/agent.json`.
    ///
    /// Returns `None` when the endpoint is unreachable or returns unexpected JSON.
    pub async fn fetch_agent_card(&self, config: &A2aAgentConfig) -> Result<AgentCard> {
        // Strip trailing slash and append the well-known path.
        let base = config.url.trim_end_matches('/');
        let card_url = format!("{}/.well-known/agent.json", base);

        debug!(url = %card_url, "Fetching A2A agent card");

        let mut req = self.http.get(&card_url);
        if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Failed to reach A2A agent at {}", card_url))?;

        if !resp.status().is_success() {
            bail!(
                "A2A agent card request failed with status {}",
                resp.status()
            );
        }

        let body: Value = resp
            .json()
            .await
            .context("Failed to parse A2A agent card as JSON")?;

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

        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Failed to reach A2A agent at {}", url))?;

        if !resp.status().is_success() {
            bail!("{}", detailed_http_error("message/send", resp).await);
        }

        let value: Value = resp
            .json()
            .await
            .context("Failed to parse A2A message/send response as JSON")?;

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

        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Failed to reach A2A agent at {}", url))?;

        if !resp.status().is_success() {
            bail!("{}", detailed_http_error("message/stream", resp).await);
        }

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
            let value: Value = resp
                .json()
                .await
                .context("Failed to parse non-streaming A2A response")?;

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

        // Parse the SSE byte stream into A2aStreamEvent items.
        let byte_stream = resp.bytes_stream();
        let event_stream = async_stream::stream! {
            use futures::StreamExt;

            let mut buffer = String::new();
            let mut stream = byte_stream;

            while let Some(result) = stream.next().await {
                let bytes = match result {
                    Ok(b) => b,
                    Err(e) => {
                        yield Err(anyhow::anyhow!("SSE stream error: {}", e));
                        return;
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&bytes).replace("\r\n", "\n"));

                // SSE events are separated by double newlines.
                while let Some(pos) = buffer.find("\n\n") {
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

        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = config.api_key.as_deref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("Failed to reach A2A agent at {}", url))?;
        if !resp.status().is_success() {
            bail!("{}", detailed_http_error("message/send", resp).await);
        }
        let value: Value = resp
            .json()
            .await
            .context("Failed to parse A2A message/send response as JSON")?;
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
}
