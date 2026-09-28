//! Fixture `padded`: the `echo` tool returns `Echo: <input>`, but the compiled `.wasm` is
//! padded to roughly 5 MiB with a non-zero, non-uniform data segment, so
//! wasm-opt/LLVM cannot fold it into a cheap `memory.fill` and the loader
//! actually reads and validates the extra bytes. Used only to measure
//! cold-load time of a large module (AGE-603 / S7).
use chatty_module_sdk::{export, Plugin, PluginMetadata, ToolCallRequest};
use chatty_module_sdk::{ToolDefinition, ToolError, ToolResult};
static PAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pad.bin"));

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
            ("padded".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("echo".into(), "Echoes its input.".into());
        let parameters_schema =
            r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "echo" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        // Touch PAD so the linker can't drop it, without doing real work.
        let checksum = *std::hint::black_box(PAD).first().unwrap_or(&0);
        Ok(ToolResult::text(format!("Echo: {input} (pad={checksum})")))
    }
}

export!(Fixture);
