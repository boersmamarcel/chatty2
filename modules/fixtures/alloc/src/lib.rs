//! Fixture `alloc`: the `alloc` tool grows a `Vec` by N MiB, N from `input` (memory cap).
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
        let (name, version, description) = ("alloc".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("alloc".into(), "Allocates N MiB.".into());
        let parameters_schema = r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "alloc" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let mib: usize =
            input.trim().parse().map_err(|e| ToolError::invalid_arguments(format!("bad MiB: {e}")))?;
        let mut v: Vec<u8> = Vec::new();
        for _ in 0..mib {
            v.extend(std::iter::repeat_n(1u8, 1 << 20));
        }
        Ok(ToolResult::text(format!("allocated {} MiB", std::hint::black_box(v).len() >> 20)))
    }
}

export!(Fixture);
