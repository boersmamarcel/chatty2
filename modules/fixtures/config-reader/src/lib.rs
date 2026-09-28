//! Fixture `config-reader`: the `get` tool returns `config::get(<input>)` as `Some("..")` or `None`.
use chatty_module_sdk::{export, Capability, ConfigKey, Plugin, PluginMetadata, ToolCallRequest};
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
            ("config-reader".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![Capability::Config];
        let config_keys = vec![ConfigKey {
            name: "greeting".into(),
            description: "A greeting to read back.".into(),
            required: false,
        }];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("get".into(), "Reads a config key.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "get" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let value = chatty_module_sdk::config::get(input.trim());
        Ok(ToolResult::text(format!("{value:?}")))
    }
}

export!(Fixture);
