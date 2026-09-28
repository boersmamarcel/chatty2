//! Echo — the reference chatty plugin (`chatty:plugin@0.3.0`).
//!
//! Three tools, each taking `{"input": string}`:
//!
//! * `echo` — returns the input unchanged
//! * `reverse` — returns the input with its characters reversed
//! * `count_words` — returns the number of whitespace-separated words
//!
//! It requests no capability: `logging`, which it uses, is always granted.
//! See `README.md` for the plugin author quickstart.

use chatty_module_sdk::{
    export, Plugin, PluginMetadata, ToolCallRequest, ToolDefinition, ToolError, ToolResult,
};

/// The echo plugin.
pub struct Echo;

impl Plugin for Echo {
    fn metadata() -> PluginMetadata {
        PluginMetadata {
            name: "echo".to_string(),
            version: "0.2.0".to_string(),
            description: "Reference plugin: echo, reverse and count_words tools".to_string(),
            requested_capabilities: vec![],
            config_keys: vec![],
        }
    }

    fn list_tools() -> Vec<ToolDefinition> {
        vec![
            tool(
                "echo",
                "Returns the input string unchanged.",
                "String to echo",
            ),
            tool(
                "reverse",
                "Returns the input string with characters in reverse order.",
                "String to reverse",
            ),
            tool(
                "count_words",
                "Returns the number of whitespace-separated words in the input string.",
                "String to count words in",
            ),
        ]
    }

    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        chatty_module_sdk::log::info(&format!("echo: invoke_tool name={}", call.name));

        let run: fn(&str) -> String = match call.name.as_str() {
            "echo" => |input| input.to_string(),
            "reverse" => |input| input.chars().rev().collect(),
            "count_words" => |input| input.split_whitespace().count().to_string(),
            other => return Err(ToolError::unknown_tool(other)),
        };
        Ok(ToolResult::text(run(&input_argument(
            &call.arguments_json,
        )?)))
    }
}

/// A tool taking one required string argument, `input`.
fn tool(name: &str, description: &str, input: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters_schema: serde_json::json!({
            "type": "object",
            "properties": {"input": {"type": "string", "description": input}},
            "required": ["input"],
        })
        .to_string(),
    }
}

/// The `input` string out of a tool's arguments (`{"input": string}`).
fn input_argument(arguments_json: &str) -> Result<String, ToolError> {
    let value: serde_json::Value = serde_json::from_str(arguments_json)
        .map_err(|e| ToolError::invalid_arguments(format!("arguments are not JSON: {e}")))?;
    value
        .get("input")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError::invalid_arguments("missing required string argument `input`"))
}

export!(Echo);
