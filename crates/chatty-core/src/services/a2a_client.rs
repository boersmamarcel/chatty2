//! A2A (Agent-to-Agent) HTTP client.
//!
//! Implements the client side of the A2A protocol:
//! - `GET /.well-known/agent.json` — discover the agent's capabilities
//! - `POST <url>` with `message/send` JSON-RPC — send a task and receive a result
//! - `POST <url>` with `message/stream` JSON-RPC — stream task updates via SSE

use anyhow::{Context, Result, bail};
use futures::stream::BoxStream;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

use crate::settings::models::a2a_store::A2aAgentConfig;

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
    },
    /// An artifact chunk (text content from the agent).
    ArtifactUpdate {
        task_id: String,
        text: String,
        last_chunk: bool,
    },
}

/// A lightweight HTTP client for remote A2A agents.
#[derive(Clone)]
pub struct A2aClient {
    http: Client,
}

impl A2aClient {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to build A2A HTTP client"),
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
            bail!("A2A message/send failed with status {}", resp.status());
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

        // Extract text from result.artifacts[0].parts[0].text
        let text = value
            .pointer("/result/artifacts/0/parts/0/text")
            .or_else(|| value.pointer("/result/artifacts/0/parts/0"))
            .and_then(|v| v.as_str())
            .or_else(|| value.pointer("/result/output").and_then(|v| v.as_str()))
            .or_else(|| value.pointer("/result").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();

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
        use futures::StreamExt;
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
            bail!("A2A message/stream failed with status {}", resp.status());
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
            use futures::TryStreamExt;

            let mut buffer = String::new();
            let mut byte_stream = byte_stream;

            while let Ok(Some(bytes)) = byte_stream.try_next().await {
                buffer.push_str(&String::from_utf8_lossy(&bytes));

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
            if !buffer.trim().is_empty() {
                if let Some(evt) = parse_sse_event(&buffer) {
                    yield Ok(evt);
                }
            }
        };

        Ok(Box::pin(event_stream))
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
        return Some(A2aStreamEvent::StatusUpdate {
            task_id,
            state,
            is_final,
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
            } => {
                assert_eq!(task_id, "task-abc");
                assert_eq!(state, "working");
                assert!(!is_final);
            }
            _ => panic!("Expected StatusUpdate"),
        }
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
