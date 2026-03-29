//! A2A (Agent-to-Agent) HTTP client.
//!
//! Implements the client side of the A2A protocol:
//! - `GET /.well-known/agent.json` — discover the agent's capabilities
//! - `POST <url>` with `message/send` JSON-RPC — send a task and receive a result

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{debug, info};

use crate::settings::models::a2a_store::A2aAgentConfig;

/// Discovered capabilities from a remote A2A agent card.
#[derive(Clone, Debug)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
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
    pub async fn fetch_agent_card(
        &self,
        config: &A2aAgentConfig,
    ) -> Result<AgentCard> {
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

        info!(
            url = %card_url,
            agent = %name,
            skill_count = skills.len(),
            "A2A agent card fetched successfully"
        );

        Ok(AgentCard {
            name,
            description,
            skills,
        })
    }

    /// Send a `message/send` JSON-RPC request to the remote A2A agent.
    ///
    /// Returns the plain-text response extracted from the task artifacts.
    pub async fn send_message(
        &self,
        config: &A2aAgentConfig,
        prompt: &str,
    ) -> Result<String> {
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
}

impl Default for A2aClient {
    fn default() -> Self {
        Self::new()
    }
}
