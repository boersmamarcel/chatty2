use chatty_module_sdk::{
    export, Plugin, PluginMetadata, ToolCallRequest, ToolDefinition, ToolError, ToolResult,
};

/// Replace `MyPlugin` with your own plugin type name.
pub struct MyPlugin;

impl Plugin for MyPlugin {
    fn metadata() -> PluginMetadata {
        PluginMetadata {
            name: "{{project-name}}".to_string(),
            version: "0.1.0".to_string(),
            description: "{{description}}".to_string(),
            // Add a `Capability` (e.g. `Capability::Llm`) for every host
            // import a tool calls; an agent spec can only grant what is
            // requested here. `logging` is always granted.
            requested_capabilities: vec![],
            config_keys: vec![],
        }
    }

    fn list_tools() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "greet".to_string(),
            description: "Greets the given name.".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "Who to greet"}},
                "required": ["name"],
            })
            .to_string(),
        }]
    }

    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        match call.name.as_str() {
            "greet" => {
                let args: serde_json::Value = serde_json::from_str(&call.arguments_json)
                    .map_err(|e| ToolError::invalid_arguments(format!("arguments are not JSON: {e}")))?;
                let name = args["name"]
                    .as_str()
                    .ok_or_else(|| ToolError::invalid_arguments("missing string argument `name`"))?;
                Ok(ToolResult::text(format!("Hello, {name}!")))
            }
            other => Err(ToolError::unknown_tool(other)),
        }
    }
}

export!(MyPlugin);
