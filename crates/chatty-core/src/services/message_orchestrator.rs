use std::path::Path;

use rig::message::UserContent;
use tracing::{debug, info};

use crate::services::auto_context::{AutoContextRequest, load_auto_context_block};
use crate::services::embedding_service::EmbeddingService;
use crate::services::memory_query::simplify_memory_query;
use crate::services::memory_service::MemoryService;
use crate::services::skill_service::SkillService;

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

/// Augment user contents with auto-retrieved memory context.
///
/// This is the shared "memory injection" step used by both frontends before
/// calling `stream_prompt()`. It:
///
/// 1. Extracts text from user contents to build a query
/// 2. Simplifies the query for better retrieval
/// 3. Calls `load_auto_context_block()` to find relevant memories and skills
/// 4. Prepends the context block to the user contents (if any matches found)
///
/// The returned contents should be sent to the LLM, while the **original**
/// contents (without the context block) should be persisted to conversation
/// history so that reopening old conversations doesn't show injected context.
pub async fn augment_with_memory(
    contents: Vec<UserContent>,
    memory_service: Option<&MemoryService>,
    embedding_service: Option<&EmbeddingService>,
    skill_service: &SkillService,
    workspace_dir: Option<&str>,
) -> Vec<UserContent> {
    let mem_svc = match memory_service {
        Some(svc) => svc,
        None => return contents,
    };

    let raw_text = extract_user_text(&contents);
    if raw_text.is_empty() {
        return contents;
    }

    let query_text = simplify_memory_query(&raw_text);
    info!(
        raw_len = raw_text.len(),
        query = %query_text,
        "Memory auto-retrieval: searching"
    );

    let workspace_skills_dir = workspace_dir.map(|d| Path::new(d).join(".claude").join("skills"));

    match load_auto_context_block(AutoContextRequest {
        memory_service: mem_svc,
        embedding_service,
        skill_service,
        query_text: &query_text,
        fallback_query_text: Some(&raw_text),
        workspace_skills_dir: workspace_skills_dir.as_deref(),
    })
    .await
    {
        Some(context_block) => {
            let mut augmented = vec![UserContent::Text(rig::completion::message::Text {
                text: context_block,
            })];
            augmented.extend(contents);
            debug!("Injected memory context into user message");
            augmented
        }
        None => contents,
    }
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

    #[tokio::test]
    async fn augment_with_memory_passthrough_without_service() {
        let contents = vec![UserContent::text("test message")];
        let skill_service = SkillService::new(None);
        let result = augment_with_memory(contents.clone(), None, None, &skill_service, None).await;
        assert_eq!(result.len(), 1);
    }
}
