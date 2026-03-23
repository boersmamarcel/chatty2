use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Base URL for the Daytona cloud API
const DAYTONA_API_BASE: &str = "https://app.daytona.io/api";

/// Default timeout for sandbox creation and code execution (seconds)
const DAYTONA_TIMEOUT_SECS: u64 = 60;

/// Maximum polling attempts when waiting for sandbox to reach "started" state (~30s)
const DAYTONA_SANDBOX_POLL_ATTEMPTS: u64 = 15;

/// Polling interval in milliseconds while waiting for sandbox start
const DAYTONA_SANDBOX_POLL_INTERVAL_MS: u64 = 2000;

// ── Tool Args / Output ──────────────────────────────────────────────────────

/// Arguments for the daytona_run tool
#[derive(Deserialize, Serialize)]
pub struct DaytonaToolArgs {
    /// The code to execute in the Daytona sandbox
    pub code: String,
    /// Programming language hint (e.g. "python", "javascript", "bash")
    #[serde(default)]
    pub language: Option<String>,
}

/// Output from the daytona_run tool
#[derive(Debug, Serialize)]
pub struct DaytonaToolOutput {
    /// The code that was executed
    pub code: String,
    /// Standard output from the code execution
    pub result: String,
    /// Exit code (0 = success)
    pub exit_code: i32,
    /// Whether the sandbox was cleaned up after use
    pub sandbox_cleaned_up: bool,
}

/// Error type for the daytona_run tool
#[derive(Debug, thiserror::Error)]
pub enum DaytonaToolError {
    #[error("Daytona error: {0}")]
    ApiError(String),
}

// ── Daytona API types ────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct SandboxResponse {
    id: String,
    /// Proxy URL used to reach the sandbox toolbox (e.g. process/code-run endpoint).
    /// Returned as "toolboxProxyUrl" in the JSON response.
    #[serde(rename = "toolboxProxyUrl")]
    toolbox_proxy_url: String,
}

/// Lightweight response used when polling sandbox state.
#[derive(Deserialize, Debug)]
struct SandboxStateResponse {
    state: String,
}

#[derive(Serialize)]
struct CodeRunRequest {
    code: String,
}

#[derive(Deserialize, Debug)]
struct CodeRunResponse {
    result: Option<String>,
    #[serde(default)]
    exit_code: i32,
}

// ── Tool implementation ──────────────────────────────────────────────────────

/// Code execution tool powered by the Daytona cloud sandbox service.
///
/// Creates an isolated Daytona sandbox, runs the provided code, returns the
/// output, and cleans up the sandbox afterwards.
#[derive(Clone)]
pub struct DaytonaTool {
    client: reqwest::Client,
    api_key: String,
    api_base: String,
}

impl DaytonaTool {
    /// Create a new DaytonaTool with the given API key.
    pub fn new(api_key: String) -> Self {
        Self::new_with_base(api_key, DAYTONA_API_BASE.to_string())
    }

    /// Create a DaytonaTool with a custom API base URL (useful for self-hosted Daytona).
    pub fn new_with_base(api_key: String, api_base: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DAYTONA_TIMEOUT_SECS))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .expect("Failed to build HTTP client");
        Self {
            client,
            api_key,
            api_base,
        }
    }

    /// Create a new Daytona sandbox and return its ID and toolbox proxy URL.
    async fn create_sandbox(&self) -> Result<(String, String), DaytonaToolError> {
        let response = self
            .client
            .post(format!("{}/sandbox", self.api_base))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to create Daytona sandbox: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(DaytonaToolError::ApiError(format!(
                "Daytona sandbox creation returned {}: {}",
                status, body
            )));
        }

        let sandbox: SandboxResponse = response.json().await.map_err(|e| {
            DaytonaToolError::ApiError(format!("Failed to parse sandbox response: {}", e))
        })?;

        Ok((sandbox.id, sandbox.toolbox_proxy_url))
    }

    /// Poll until the sandbox reaches the "started" state (up to ~30 seconds).
    async fn wait_for_started(&self, sandbox_id: &str) -> Result<(), DaytonaToolError> {
        for attempt in 0..DAYTONA_SANDBOX_POLL_ATTEMPTS {
            let response = self
                .client
                .get(format!("{}/sandbox/{}", self.api_base, sandbox_id))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .send()
                .await
                .map_err(|e| {
                    DaytonaToolError::ApiError(format!(
                        "Failed to poll sandbox state: {}",
                        e
                    ))
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "(failed to read body)".to_string());
                return Err(DaytonaToolError::ApiError(format!(
                    "Daytona sandbox state poll returned {}: {}",
                    status, body
                )));
            }

            let state_resp: SandboxStateResponse = response.json().await.map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to parse sandbox state: {}", e))
            })?;

            info!(
                sandbox_id,
                attempt,
                state = %state_resp.state,
                "Waiting for Daytona sandbox to start"
            );

            match state_resp.state.as_str() {
                "started" => return Ok(()),
                "error" => {
                    return Err(DaytonaToolError::ApiError(format!(
                        "Daytona sandbox {} entered error state",
                        sandbox_id
                    )));
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        DAYTONA_SANDBOX_POLL_INTERVAL_MS,
                    ))
                    .await;
                }
            }
        }

        Err(DaytonaToolError::ApiError(format!(
            "Daytona sandbox {} did not reach 'started' state in time",
            sandbox_id
        )))
    }

    /// Run code in an existing Daytona sandbox via the toolbox proxy URL.
    async fn run_code(
        &self,
        toolbox_proxy_url: &str,
        code: &str,
    ) -> Result<CodeRunResponse, DaytonaToolError> {
        let request = CodeRunRequest {
            code: code.to_string(),
        };

        let response = self
            .client
            .post(format!("{}/process/code-run", toolbox_proxy_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                DaytonaToolError::ApiError(format!("Failed to run code in sandbox: {}", e))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(DaytonaToolError::ApiError(format!(
                "Daytona code execution returned {}: {}",
                status, body
            )));
        }

        let run_response: CodeRunResponse = response.json().await.map_err(|e| {
            DaytonaToolError::ApiError(format!("Failed to parse code run response: {}", e))
        })?;

        Ok(run_response)
    }

    /// Delete a Daytona sandbox to free resources.
    async fn delete_sandbox(&self, sandbox_id: &str) -> bool {
        let result = self
            .client
            .delete(format!("{}/sandbox/{}", self.api_base, sandbox_id))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                info!(sandbox_id, "Daytona sandbox deleted");
                true
            }
            Ok(resp) => {
                warn!(
                    sandbox_id,
                    status = %resp.status(),
                    "Failed to delete Daytona sandbox"
                );
                false
            }
            Err(e) => {
                warn!(sandbox_id, error = %e, "Error deleting Daytona sandbox");
                false
            }
        }
    }
}

impl Tool for DaytonaTool {
    const NAME: &'static str = "daytona_run";
    type Error = DaytonaToolError;
    type Args = DaytonaToolArgs;
    type Output = DaytonaToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "daytona_run".to_string(),
            description: "Execute code in an isolated Daytona cloud sandbox. \
                         Creates a secure, ephemeral sandbox environment, runs your code, \
                         returns the output, and cleans up automatically. \
                         Use this for running code snippets, scripts, or commands in a \
                         fully isolated cloud environment with internet access."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The code or script to execute in the sandbox"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language hint (e.g. 'python', 'javascript', 'bash'). Optional."
                    }
                },
                "required": ["code"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let code = args.code.trim().to_string();
        if code.is_empty() {
            return Err(DaytonaToolError::ApiError(
                "Code cannot be empty".to_string(),
            ));
        }

        let lang = args
            .language
            .as_deref()
            .unwrap_or("unknown")
            .to_string();

        info!(language = %lang, "Creating Daytona sandbox");
        let (sandbox_id, toolbox_proxy_url) = self.create_sandbox().await?;
        info!(sandbox_id = %sandbox_id, language = %lang, "Daytona sandbox created, waiting for start");

        // Wait for the sandbox to reach "started" state before running code.
        if let Err(e) = self.wait_for_started(&sandbox_id).await {
            // Clean up best-effort before returning the error.
            self.delete_sandbox(&sandbox_id).await;
            return Err(e);
        }

        info!(sandbox_id = %sandbox_id, language = %lang, "Daytona sandbox started, running code");
        let run_result = self.run_code(&toolbox_proxy_url, &code).await;

        // Always attempt cleanup regardless of execution result
        let cleaned_up = self.delete_sandbox(&sandbox_id).await;

        let run_response = match run_result {
            Ok(resp) => resp,
            Err(e) => {
                // If cleanup also failed, log a warning so users know the sandbox may persist
                if !cleaned_up {
                    warn!(
                        sandbox_id = %sandbox_id,
                        "Daytona code execution failed and sandbox cleanup also failed — \
                         the sandbox may still be running in your account"
                    );
                }
                return Err(e);
            }
        };

        let result = run_response.result.unwrap_or_else(|| "(no output)".to_string());

        if run_response.exit_code != 0 {
            warn!(
                sandbox_id = %sandbox_id,
                exit_code = run_response.exit_code,
                "Daytona code execution exited with non-zero code"
            );
        } else {
            info!(sandbox_id = %sandbox_id, "Daytona code execution completed successfully");
        }

        Ok(DaytonaToolOutput {
            code,
            result,
            exit_code: run_response.exit_code,
            sandbox_cleaned_up: cleaned_up,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_daytona_tool_definition() {
        let tool = DaytonaTool::new("test-key".into());
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "daytona_run");
        assert!(def.description.contains("sandbox"));
    }

    #[tokio::test]
    async fn test_daytona_tool_empty_code() {
        let tool = DaytonaTool::new("test-key".into());
        let args = DaytonaToolArgs {
            code: "   ".to_string(),
            language: None,
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }
}
