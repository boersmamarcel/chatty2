//! Fixture `fuel-meter`: each `burn` call runs N loop iterations, N from `input` (default 5,000,000).
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
        let (name, version, description) = ("fuel-meter".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("burn".into(), "Burns N loop iterations.".into());
        let parameters_schema = r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "burn" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let n: u64 = input.trim().parse().unwrap_or(5_000_000);
        let mut x: u64 = 0;
        for i in 0..n {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        Ok(ToolResult::text(format!("burned {n} iterations ({x})")))
    }
}

export!(Fixture);
