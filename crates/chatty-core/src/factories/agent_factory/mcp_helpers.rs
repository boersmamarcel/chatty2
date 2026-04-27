use std::collections::HashSet;

/// MCP listing tool, always enabled when MCP servers are configured.
pub(super) struct McpTools {
    pub list: Option<crate::tools::ListMcpTool>,
}

impl McpTools {
    #[cfg(test)]
    pub fn none() -> Self {
        Self { list: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.list.is_some()
    }
}

/// Deduplicate MCP tools by name across all servers.
///
/// When multiple MCP servers are configured, they may provide tools with the same name.
/// LLM providers (Anthropic, OpenAI, etc.) require unique tool names, so this function
/// deduplicates by keeping the first occurrence of each tool name and logging skipped duplicates.
pub(super) fn deduplicate_mcp_tools(
    mcp_tools: Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>,
    reserved_tool_names: &HashSet<String>,
) -> Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)> {
    let mut seen_tool_names = reserved_tool_names.clone();
    let mut result = Vec::new();

    for (server_name, tools, sink) in mcp_tools {
        let mut deduped_tools = Vec::new();
        let mut skipped_count = 0;
        let total_tools = tools.len();

        for tool in tools {
            if seen_tool_names.insert(tool.name.to_string()) {
                deduped_tools.push(tool);
            } else {
                tracing::warn!(
                    server = %server_name,
                    tool_name = %tool.name,
                    "Skipping duplicate MCP tool name"
                );
                skipped_count += 1;
            }
        }

        if skipped_count > 0 {
            tracing::info!(
                server = %server_name,
                total = total_tools,
                kept = deduped_tools.len(),
                skipped = skipped_count,
                "Deduplicated tools from MCP server"
            );
        }

        if !deduped_tools.is_empty() {
            result.push((server_name, deduped_tools, sink));
        }
    }

    result
}

pub(super) fn filter_mcp_tool_info(
    mcp_tool_info: Vec<(String, String, String)>,
    reserved_tool_names: &HashSet<String>,
) -> Vec<(String, String, String)> {
    let mut seen_tool_names = reserved_tool_names.clone();
    let mut filtered = Vec::new();

    for (server_name, tool_name, tool_description) in mcp_tool_info {
        if seen_tool_names.insert(tool_name.clone()) {
            filtered.push((server_name, tool_name, tool_description));
        } else {
            tracing::warn!(
                server = %server_name,
                tool_name = %tool_name,
                "Skipping duplicate MCP tool from list_tools inventory"
            );
        }
    }

    filtered
}

/// Recursively strip `"format"` fields from a JSON Schema object.
///
/// OpenAI strict-mode function calling does not support the `"format"` keyword
/// (e.g., `"format": "uri"`). MCP tool schemas may include these, so we strip
/// them before sending to OpenAI / Azure OpenAI.
///
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"` (not fixed as of v0.32).
fn strip_format_from_schema(schema: &mut serde_json::Map<String, serde_json::Value>) {
    schema.remove("format");

    if let Some(serde_json::Value::Object(props)) = schema.get_mut("properties") {
        for prop_value in props.values_mut() {
            if let serde_json::Value::Object(prop_obj) = prop_value {
                strip_format_from_schema(prop_obj);
            }
        }
    }
    if let Some(serde_json::Value::Object(items)) = schema.get_mut("items") {
        strip_format_from_schema(items);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(serde_json::Value::Array(variants)) = schema.get_mut(key) {
            for variant in variants.iter_mut() {
                if let serde_json::Value::Object(obj) = variant {
                    strip_format_from_schema(obj);
                }
            }
        }
    }
    if let Some(serde_json::Value::Object(defs)) = schema.get_mut("$defs") {
        for def in defs.values_mut() {
            if let serde_json::Value::Object(def_obj) = def {
                strip_format_from_schema(def_obj);
            }
        }
    }
}

/// Sanitize MCP tool schemas for OpenAI compatibility.
///
/// Strips unsupported JSON Schema keywords (like `"format"`) that OpenAI's
/// strict-mode function calling rejects.
///
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"` (not fixed as of v0.32).
pub(super) fn sanitize_mcp_tools_for_openai(
    mcp_tools: Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>>,
) -> Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
    mcp_tools.map(|servers| {
        servers
            .into_iter()
            .map(|(name, tools, sink)| {
                let sanitized_tools = tools
                    .into_iter()
                    .map(|mut tool| {
                        let mut schema = (*tool.input_schema).clone();
                        strip_format_from_schema(&mut schema);
                        tool.input_schema = std::sync::Arc::new(schema);
                        tool
                    })
                    .collect();
                (name, sanitized_tools, sink)
            })
            .collect()
    })
}

macro_rules! build_with_mcp_tools {
    ($builder:expr, $mcp_tools:expr, $reserved_tool_names:expr) => {{
        match $mcp_tools {
            Some(tools_list) => {
                let deduped = $crate::factories::agent_factory::mcp_helpers::deduplicate_mcp_tools(
                    tools_list,
                    $reserved_tool_names,
                );
                let mut iter = deduped
                    .into_iter()
                    .filter(|(_name, t, _sink)| !t.is_empty());
                if let Some((_first_name, first_tools, first_sink)) = iter.next() {
                    let mut b = $builder.rmcp_tools(first_tools, first_sink);
                    for (_name, tools, sink) in iter {
                        b = b.rmcp_tools(tools, sink);
                    }
                    b.build()
                } else {
                    $builder.build()
                }
            }
            None => $builder.build(),
        }
    }};
}

pub(super) use build_with_mcp_tools;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    // ── filter_mcp_tool_info ────────────────────────────────────────────────

    #[test]
    fn filter_mcp_tool_info_no_duplicates() {
        let reserved = HashSet::new();
        let input = vec![
            ("server-a".into(), "tool_1".into(), "desc".into()),
            ("server-b".into(), "tool_2".into(), "desc".into()),
        ];
        let result = filter_mcp_tool_info(input, &reserved);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_mcp_tool_info_removes_reserved_names() {
        let reserved: HashSet<String> = ["read_file".into()].into();
        let input = vec![
            ("server-a".into(), "read_file".into(), "conflicts".into()),
            ("server-a".into(), "custom_tool".into(), "ok".into()),
        ];
        let result = filter_mcp_tool_info(input, &reserved);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, "custom_tool");
    }

    #[test]
    fn filter_mcp_tool_info_removes_cross_server_duplicates() {
        let reserved = HashSet::new();
        let input = vec![
            ("server-a".into(), "shared_tool".into(), "first".into()),
            ("server-b".into(), "shared_tool".into(), "duplicate".into()),
            ("server-b".into(), "unique_tool".into(), "ok".into()),
        ];
        let result = filter_mcp_tool_info(input, &reserved);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "server-a");
        assert_eq!(result[0].1, "shared_tool");
        assert_eq!(result[1].1, "unique_tool");
    }

    #[test]
    fn filter_mcp_tool_info_empty_input() {
        let result = filter_mcp_tool_info(vec![], &HashSet::new());
        assert!(result.is_empty());
    }

    // ── strip_format_from_schema ────────────────────────────────────────────

    #[test]
    fn strip_format_removes_top_level_format() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "string",
                "format": "uri"
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        assert!(!schema.contains_key("format"));
        assert_eq!(schema["type"], "string");
    }

    #[test]
    fn strip_format_removes_nested_in_properties() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "format": "uri" },
                    "name": { "type": "string" }
                }
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        let props = schema["properties"].as_object().unwrap();
        assert!(!props["url"].as_object().unwrap().contains_key("format"));
        assert_eq!(props["name"]["type"], "string");
    }

    #[test]
    fn strip_format_removes_from_items() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "array",
                "items": { "type": "string", "format": "email" }
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        assert!(!schema["items"].as_object().unwrap().contains_key("format"));
    }

    #[test]
    fn strip_format_removes_from_any_of() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "anyOf": [
                    { "type": "string", "format": "uri" },
                    { "type": "integer" }
                ]
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        let variants = schema["anyOf"].as_array().unwrap();
        assert!(!variants[0].as_object().unwrap().contains_key("format"));
    }

    #[test]
    fn strip_format_removes_from_defs() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "$defs": {
                    "Url": { "type": "string", "format": "uri" }
                }
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        let defs = schema["$defs"].as_object().unwrap();
        assert!(!defs["Url"].as_object().unwrap().contains_key("format"));
    }

    #[test]
    fn strip_format_preserves_non_format_fields() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "string",
                "description": "A URL",
                "format": "uri",
                "minLength": 1
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        assert!(!schema.contains_key("format"));
        assert_eq!(schema["description"], "A URL");
        assert_eq!(schema["minLength"], 1);
    }

    #[test]
    fn strip_format_handles_deeply_nested() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": {
                            "deep": { "type": "string", "format": "date-time" }
                        }
                    }
                }
            }))
            .unwrap();
        strip_format_from_schema(&mut schema);
        let deep = &schema["properties"]["nested"]["properties"]["deep"];
        assert!(!deep.as_object().unwrap().contains_key("format"));
    }

    #[test]
    fn strip_format_no_op_when_no_format() {
        let mut schema: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                }
            }))
            .unwrap();
        let original = schema.clone();
        strip_format_from_schema(&mut schema);
        assert_eq!(schema, original);
    }
}
