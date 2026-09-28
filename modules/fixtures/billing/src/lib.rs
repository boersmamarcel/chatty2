//! Fixture `billing`: the `settle` tool reports `<input>` (`"<in> <out>"` tokens) through
//! `hive-billing-sdk`, proving that crate and `chatty-module-sdk` link into
//! one component (PL-E7 S8.5).
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
        let (name, version, description) = ("billing".into(), "0.1.0".into(), "Test fixture.".into());
        let requested_capabilities = vec![Capability::Billing];
        let config_keys = vec![];
        PluginMetadata { name, version, description, requested_capabilities, config_keys }
    }
    fn list_tools() -> Vec<ToolDefinition> {
        let (name, description) = ("settle".into(), "Reports token usage to the host's billing.".into());
        let parameters_schema = r#"{"type":"object","properties":{"input":{"type":"string"}}}"#.into();
        vec![ToolDefinition { name, description, parameters_schema }]
    }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
        if call.name != "settle" {
            return Err(ToolError::unknown_tool(&call.name));
        }
        let input = input(&call)?;
        let mut tokens = input.split_whitespace().map(str::parse::<i64>);
        let (Some(Ok(input_tokens)), Some(Ok(output_tokens))) = (tokens.next(), tokens.next()) else {
            return Err(ToolError::invalid_arguments("expected `<input tokens> <output tokens>`"));
        };
        hive_billing_sdk::report_usage_simple(input_tokens, output_tokens).map_err(ToolError::denied)?;
        Ok(ToolResult::text("settled"))
    }
}

export!(Fixture);
