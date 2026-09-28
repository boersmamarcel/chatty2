//! Fixture `tool-args`: the `echo_args` tool returns its raw `arguments-json` unchanged.
use chatty_module_sdk::{export, Plugin, PluginMetadata, ToolCallRequest, ToolDefinition};
use chatty_module_sdk::{ToolError, ToolResult};

struct Fixture;

impl Plugin for Fixture {
    fn metadata() -> PluginMetadata {
        let (name, version, description) = ("tool-args".into(), "0.1.0".into(), "Test fixture.".into());
        PluginMetadata { name, version, description, requested_capabilities: vec![], config_keys: vec![] }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("echo_args".into(), "Returns its raw arguments.".into());
        let parameters_schema = r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "echo_args" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        Ok(ToolResult::text(call.arguments_json))
    }
}

export!(Fixture);
