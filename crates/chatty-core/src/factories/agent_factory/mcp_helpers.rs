use std::collections::HashSet;

/// All four MCP management tools bundled together.
///
/// All four are gated on the same `mcp_service_tool_enabled` setting, so they
/// are always constructed (or not) as a unit.
pub(super) struct McpTools {
    pub add: Option<crate::tools::AddMcpTool>,
    pub delete: Option<crate::tools::DeleteMcpTool>,
    pub edit: Option<crate::tools::EditMcpTool>,
    pub list: Option<crate::tools::ListMcpTool>,
}

impl McpTools {
    pub fn none() -> Self {
        Self {
            add: None,
            delete: None,
            edit: None,
            list: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.add.is_some()
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
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"`.
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
/// TODO(#127): Remove once rig-core's `sanitize_schema()` strips `"format"`.
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
