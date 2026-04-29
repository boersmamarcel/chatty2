use rig::message::UserContent;
use tracing::info;

/// Extract the text portion of user contents for memory query.
///
/// This filters out non-text content (images, PDFs) and joins text fragments.
/// Shared between GPUI (which had this in token_budget/manager.rs) and TUI.
pub fn extract_user_text(contents: &[UserContent]) -> String {
    contents
        .iter()
        .filter_map(|c| match c {
            UserContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Gather MCP tools from the service, returning `None` when no tools are available.
///
/// This wraps the common pattern used by both frontends:
/// - Call `get_all_tools_with_sinks()`
/// - Log the count
/// - Return `None` for empty tool sets or errors
pub async fn gather_mcp_tools(
    mcp_service: &crate::services::mcp_service::McpService,
) -> Option<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
    match mcp_service.get_all_tools_with_sinks().await {
        Ok(tools) if !tools.is_empty() => {
            info!(count = tools.len(), "MCP tools loaded");
            Some(tools)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = ?e, "Failed to load MCP tools");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_user_text_joins_fragments() {
        let contents = vec![UserContent::text("hello"), UserContent::text("world")];
        assert_eq!(extract_user_text(&contents), "hello world");
    }

    #[test]
    fn extract_user_text_empty_for_no_text() {
        let contents: Vec<UserContent> = vec![];
        assert_eq!(extract_user_text(&contents), "");
    }
}
