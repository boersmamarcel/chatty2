//! Firefox DevTools protocol client for communicating with Verso's built-in debug server.
//!
//! Verso exposes a Firefox Remote Debug Protocol server on a configurable TCP port.
//! This module implements the minimal subset needed for browser automation:
//! - Connecting to the debug server
//! - Navigating to URLs
//! - Evaluating JavaScript
//! - Querying page state

use crate::error::BrowserError;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Client for the Firefox Remote Debug Protocol used by Verso's DevTools server.
pub struct DevToolsClient {
    /// TCP stream to the DevTools server, wrapped in a mutex for concurrent access.
    stream: Mutex<Option<BufReader<TcpStream>>>,
    /// Monotonically increasing request ID for correlating responses.
    next_id: AtomicU64,
    /// Port the DevTools server is listening on.
    port: u16,
}

/// A message sent to the DevTools server.
#[derive(Debug, Serialize)]
struct DevToolsRequest {
    /// Unique request identifier.
    id: u64,
    /// Method to invoke (e.g., "navigateTo", "evaluateJSAsync").
    method: String,
    /// Method-specific parameters.
    params: serde_json::Value,
    /// Target actor ID.
    to: String,
}

/// A response from the DevTools server.
#[derive(Debug, Deserialize)]
pub struct DevToolsResponse {
    /// The request ID this response corresponds to (not always present).
    #[serde(default)]
    pub id: Option<u64>,
    /// Error message, if the request failed.
    pub error: Option<String>,
    /// Result payload (method-specific).
    #[serde(default)]
    pub result: serde_json::Value,
    /// Actor that sent this response.
    #[serde(default)]
    pub from: Option<String>,
}

/// Result of evaluating JavaScript in the page context.
#[derive(Debug, Clone)]
pub struct JsEvalResult {
    /// The value returned by the evaluated expression, as a JSON string.
    pub value: String,
    /// Whether evaluation threw an exception.
    pub is_exception: bool,
}

impl DevToolsClient {
    /// Create a new client targeting the given DevTools port.
    ///
    /// Does not connect immediately — call [`connect`] to establish the TCP connection.
    pub fn new(port: u16) -> Self {
        Self {
            stream: Mutex::new(None),
            next_id: AtomicU64::new(1),
            port,
        }
    }

    /// Connect to the DevTools server.
    ///
    /// Retries up to `max_retries` times with a 500ms delay between attempts,
    /// to allow the versoview process time to start its DevTools server.
    pub async fn connect(&self, max_retries: u32) -> Result<(), BrowserError> {
        let addr = format!("127.0.0.1:{}", self.port);
        debug!(addr = %addr, "Connecting to DevTools server");

        for attempt in 1..=max_retries {
            match TcpStream::connect(&addr).await {
                Ok(stream) => {
                    let reader = BufReader::new(stream);
                    let mut guard = self.stream.lock().await;
                    *guard = Some(reader);
                    debug!(attempt, "Connected to DevTools server");
                    return Ok(());
                }
                Err(e) => {
                    if attempt < max_retries {
                        debug!(attempt, error = %e, "DevTools connection attempt failed, retrying...");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    } else {
                        return Err(BrowserError::DevToolsConnectionFailed(format!(
                            "Failed to connect to {}:{} after {} attempts: {}",
                            "127.0.0.1", self.port, max_retries, e
                        )));
                    }
                }
            }
        }

        unreachable!()
    }

    /// Send a raw request to the DevTools server and return the response.
    pub async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
        actor: &str,
    ) -> Result<DevToolsResponse, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = DevToolsRequest {
            id,
            method: method.to_string(),
            params,
            to: actor.to_string(),
        };

        let payload =
            serde_json::to_string(&request).map_err(|e| BrowserError::DevToolsProtocol(e.to_string()))?;

        let mut guard = self.stream.lock().await;
        let stream = guard
            .as_mut()
            .ok_or(BrowserError::DevToolsConnectionFailed(
                "Not connected".to_string(),
            ))?;

        // Firefox DevTools protocol uses length-prefixed JSON messages.
        // Format: `<length>:<json>`
        let msg = format!("{}:{}", payload.len(), payload);
        stream
            .get_mut()
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| BrowserError::DevToolsProtocol(format!("Write failed: {}", e)))?;

        // Read the response (also length-prefixed)
        let response = Self::read_message(stream).await?;
        debug!(id, method, "DevTools response received");

        Ok(response)
    }

    /// Read a single length-prefixed JSON message from the stream.
    async fn read_message(
        stream: &mut BufReader<TcpStream>,
    ) -> Result<DevToolsResponse, BrowserError> {
        // Read until we get the length prefix (digits followed by ':')
        let mut length_buf = String::new();
        loop {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .map_err(|e| BrowserError::DevToolsProtocol(format!("Read failed: {}", e)))?;
            let ch = byte[0] as char;
            if ch == ':' {
                break;
            }
            if ch.is_ascii_digit() {
                length_buf.push(ch);
            } else {
                return Err(BrowserError::DevToolsProtocol(format!(
                    "Unexpected character in length prefix: '{}'",
                    ch
                )));
            }
        }

        let length: usize = length_buf
            .parse()
            .map_err(|e| BrowserError::DevToolsProtocol(format!("Invalid length: {}", e)))?;

        // Read exactly `length` bytes of JSON
        let mut json_buf = vec![0u8; length];
        stream
            .read_exact(&mut json_buf)
            .await
            .map_err(|e| BrowserError::DevToolsProtocol(format!("Read failed: {}", e)))?;

        let response: DevToolsResponse = serde_json::from_slice(&json_buf)
            .map_err(|e| BrowserError::DevToolsProtocol(format!("Invalid JSON: {}", e)))?;

        Ok(response)
    }

    /// Navigate the tab to the given URL.
    pub async fn navigate(&self, url: &str, actor: &str) -> Result<DevToolsResponse, BrowserError> {
        self.send_request(
            "navigateTo",
            serde_json::json!({ "url": url }),
            actor,
        )
        .await
    }

    /// Evaluate a JavaScript expression in the page context and return the result.
    pub async fn evaluate_js(
        &self,
        expression: &str,
        actor: &str,
    ) -> Result<JsEvalResult, BrowserError> {
        let response = self
            .send_request(
                "evaluateJSAsync",
                serde_json::json!({ "text": expression }),
                actor,
            )
            .await?;

        if let Some(err) = response.error {
            return Err(BrowserError::JsEvalError(err));
        }

        // Parse the result from the DevTools response
        let value = if response.result.is_null() {
            "undefined".to_string()
        } else {
            serde_json::to_string(&response.result)
                .unwrap_or_else(|_| "undefined".to_string())
        };

        let is_exception = response
            .result
            .get("exceptionMessage")
            .is_some();

        Ok(JsEvalResult {
            value,
            is_exception,
        })
    }

    /// Check if the client is currently connected.
    pub async fn is_connected(&self) -> bool {
        self.stream.lock().await.is_some()
    }

    /// Disconnect from the DevTools server.
    pub async fn disconnect(&self) {
        let mut guard = self.stream.lock().await;
        if let Some(stream) = guard.take() {
            let mut tcp = stream.into_inner();
            if let Err(e) = tcp.shutdown().await {
                warn!(error = %e, "Error shutting down DevTools connection");
            }
        }
    }

    /// Return the port this client targets.
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_devtools_request_serialization() {
        let req = DevToolsRequest {
            id: 1,
            method: "navigateTo".to_string(),
            params: serde_json::json!({ "url": "https://example.com" }),
            to: "tab1".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"navigateTo\""));
        assert!(json.contains("\"url\":\"https://example.com\""));
    }

    #[test]
    fn test_devtools_response_deserialization() {
        let json = r#"{"id":1,"result":{"type":"navigated"},"from":"tab1"}"#;
        let resp: DevToolsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert_eq!(resp.from.as_deref(), Some("tab1"));
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_devtools_response_with_error() {
        let json = r#"{"id":2,"error":"unknown actor","from":"root"}"#;
        let resp: DevToolsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error.as_deref(), Some("unknown actor"));
    }

    #[test]
    fn test_client_creation() {
        let client = DevToolsClient::new(6080);
        assert_eq!(client.port(), 6080);
    }
}
