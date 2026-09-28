//! Fixture `sleep`: the `sleep` tool sleeps `input` milliseconds through WASI
//! (`std::thread::sleep`, i.e. `wasi:clocks` subscribe plus `wasi:io/poll`),
//! then returns `slept <ms> ms` (AGE-706).
use chatty_module_sdk::{export, Plugin, PluginMetadata, ToolCallRequest};
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
        let (name, version, description) = ("sleep".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("sleep".into(), "Sleeps `input` milliseconds.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "sleep" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let ms: u64 = input(&call)?.parse().map_err(|e| {
            ToolError::invalid_arguments(format!("input is not a millisecond count: {e}"))
        })?;
        std::thread::sleep(std::time::Duration::from_millis(ms));
        Ok(ToolResult { content: format!("slept {ms} ms"), usage: None })
    }
}

export!(Fixture);
