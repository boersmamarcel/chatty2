//! Fixture `threads`: the `spawn` tool tries to spawn a thread and uses atomics, reporting what the
//! host allowed.
use chatty_module_sdk::{export, Plugin, PluginMetadata, ToolCallRequest};
use chatty_module_sdk::{ToolDefinition, ToolError, ToolResult};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
            ("threads".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("spawn".into(), "Tries to spawn a thread.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "spawn" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let _ = input;
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let spawned = std::thread::Builder::new()
            .spawn(move || c.fetch_add(1, Ordering::SeqCst))
            .map(|h| h.join().is_ok());
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ToolResult::text(format!("spawn: {spawned:?}; counter: {n}")))
    }
}

export!(Fixture);
