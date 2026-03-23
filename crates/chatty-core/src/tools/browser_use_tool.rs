use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Base URL for the browser-use cloud API
const BROWSER_USE_API_BASE: &str = "https://api.browser-use.com/api/v1";

/// Default polling interval in milliseconds
const POLL_INTERVAL_MS: u64 = 2000;

/// Maximum number of polling attempts before timing out (~2 minutes)
const MAX_POLL_ATTEMPTS: u64 = 60;

// ── Tool Args / Output ──────────────────────────────────────────────────────

/// Arguments for the browser_use tool
#[derive(Deserialize, Serialize)]
pub struct BrowserUseToolArgs {
    /// A natural-language description of the browser task to perform
    pub task: String,
}

/// Output from the browser_use tool
#[derive(Debug, Serialize)]
pub struct BrowserUseToolOutput {
    /// The task that was executed
    pub task: String,
    /// The result returned by the browser agent
    pub output: String,
    /// Final status of the task
    pub status: String,
}

/// Error type for the browser_use tool
#[derive(Debug, thiserror::Error)]
pub enum BrowserUseToolError {
    #[error("Browser-use error: {0}")]
    ApiError(String),
}

// ── browser-use Cloud API types ─────────────────────────────────────────────

#[derive(Serialize)]
struct RunTaskRequest {
    task: String,
}

#[derive(Deserialize, Debug)]
struct RunTaskResponse {
    id: String,
}

#[derive(Deserialize, Debug)]
struct TaskStatusResponse {
    status: String,
    output: Option<String>,
}

// ── Tool implementation ─────────────────────────────────────────────────────

/// Browser automation tool powered by the browser-use cloud service.
///
/// Submits a natural-language task to the browser-use cloud API and polls
/// for completion, returning the agent's output.
#[derive(Clone)]
pub struct BrowserUseTool {
    client: reqwest::Client,
    api_key: String,
}

impl BrowserUseTool {
    /// Create a new BrowserUseTool with the given API key.
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Chatty/1.0 (Desktop AI Assistant)")
            .build()
            .expect("Failed to build HTTP client");
        Self { client, api_key }
    }

    /// Submit a task to the browser-use cloud API and return the task ID.
    async fn submit_task(&self, task: &str) -> Result<String, BrowserUseToolError> {
        let request = RunTaskRequest {
            task: task.to_string(),
        };

        let response = self
            .client
            .post(format!("{}/run-task", BROWSER_USE_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| BrowserUseToolError::ApiError(format!("Failed to submit task: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_string());
            return Err(BrowserUseToolError::ApiError(format!(
                "browser-use API returned {}: {}",
                status, body
            )));
        }

        let run_response: RunTaskResponse = response.json().await.map_err(|e| {
            BrowserUseToolError::ApiError(format!("Failed to parse run-task response: {}", e))
        })?;

        Ok(run_response.id)
    }

    /// Poll the task status until it completes or times out.
    async fn poll_task(&self, task_id: &str) -> Result<TaskStatusResponse, BrowserUseToolError> {
        for attempt in 0..MAX_POLL_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;

            let response = self
                .client
                .get(format!("{}/task/{}", BROWSER_USE_API_BASE, task_id))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .send()
                .await
                .map_err(|e| {
                    BrowserUseToolError::ApiError(format!("Failed to poll task status: {}", e))
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "(failed to read body)".to_string());
                return Err(BrowserUseToolError::ApiError(format!(
                    "browser-use status API returned {}: {}",
                    status, body
                )));
            }

            let status_response: TaskStatusResponse = response.json().await.map_err(|e| {
                BrowserUseToolError::ApiError(format!("Failed to parse task status: {}", e))
            })?;

            info!(
                task_id,
                attempt,
                status = %status_response.status,
                "browser-use task status"
            );

            match status_response.status.as_str() {
                "finished" | "failed" | "stopped" => {
                    return Ok(status_response);
                }
                _ => {
                    // still running — keep polling
                }
            }
        }

        Err(BrowserUseToolError::ApiError(format!(
            "Task {} timed out after {} polling attempts",
            task_id, MAX_POLL_ATTEMPTS
        )))
    }
}

impl Tool for BrowserUseTool {
    const NAME: &'static str = "browser_use";
    type Error = BrowserUseToolError;
    type Args = BrowserUseToolArgs;
    type Output = BrowserUseToolOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "browser_use".to_string(),
            description: "Automate browser tasks using the browser-use cloud service. \
                         Describe what you want the browser agent to do in natural language \
                         (e.g., 'go to example.com and find the contact email', \
                         'search for the latest Python release on python.org'). \
                         The agent controls a real browser and returns the result."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "A natural-language description of the browser task to perform. \
                                        Be specific about what information to retrieve or actions to take."
                    }
                },
                "required": ["task"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let task = args.task.trim().to_string();
        if task.is_empty() {
            return Err(BrowserUseToolError::ApiError(
                "Task description cannot be empty".to_string(),
            ));
        }

        info!(task = %task, "Submitting browser-use task");
        let task_id = self.submit_task(&task).await?;
        info!(task_id = %task_id, "browser-use task submitted, polling for completion");

        let status_response = self.poll_task(&task_id).await?;

        let output = status_response.output.unwrap_or_else(|| {
            if status_response.status == "failed" {
                "Task failed without output.".to_string()
            } else {
                "Task completed without output.".to_string()
            }
        });

        if status_response.status == "failed" {
            warn!(task_id = %task_id, "browser-use task failed");
        } else {
            info!(task_id = %task_id, "browser-use task completed successfully");
        }

        Ok(BrowserUseToolOutput {
            task,
            output,
            status: status_response.status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_browser_use_tool_definition() {
        let tool = BrowserUseTool::new("test-key".into());
        let def = tool.definition("test".to_string()).await;
        assert_eq!(def.name, "browser_use");
        assert!(def.description.contains("browser"));
    }

    #[tokio::test]
    async fn test_browser_use_tool_empty_task() {
        let tool = BrowserUseTool::new("test-key".into());
        let args = BrowserUseToolArgs {
            task: "  ".to_string(),
        };
        let result = tool.call(args).await;
        assert!(result.is_err());
    }
}
