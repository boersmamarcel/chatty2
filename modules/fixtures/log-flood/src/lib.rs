//! Fixture `log-flood`: the `flood` tool emits N log lines, N from `input` (default 100,000).
use chatty_module_sdk::{export, Capability, Plugin, PluginMetadata, ToolCallRequest};
use chatty_module_sdk::{ToolDefinition, ToolError, ToolResult};

struct Fixture;

/// The call's `input` argument (`{"input": "..."}`), or `""` when absent.
fn input(call: &ToolCallRequest) -> Result<String, ToolError> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments_json)
        .map_err(|e| ToolError::invalid_arguments(format!("arguments are not JSON: {e}")))?;
    Ok(args.get("input").and_then(|v| v.as_str()).unwrap_or_default().to_string())
}

impl Plugin for Fixture {
    fn metadata() -> PluginMetadata {
        let (name, version, description) =
            ("log-flood".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![Capability::Logging];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("flood".into(), "Logs N lines.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "flood" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let n: u64 = input.trim().parse().unwrap_or(100_000);
        for i in 0..n {
            chatty_module_sdk::log::info(&format!("log-flood line {i}"));
        }
        Ok(ToolResult::text(format!("logged {n} lines")))
    }
}

export!(Fixture);
