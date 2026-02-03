use anyhow::{Result, anyhow};
use rig::completion::Message;
use rig::completion::Prompt;
use rig::completion::message::AssistantContent;
use rig::message::UserContent;
use tracing::{debug, error};

use crate::chatty::factories::AgentClient;

/// Extract text from UserContent
fn extract_text_from_user_content(content: &UserContent) -> Option<String> {
    match content {
        UserContent::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

/// Extract text from AssistantContent
fn extract_text_from_assistant_content(content: &AssistantContent) -> Option<String> {
    match content {
        AssistantContent::Text(text) => Some(text.text.clone()),
        _ => None,
    }
}

/// Truncate text to max length
fn truncate_text(text: &str, max_len: usize) -> String {
    text.chars().take(max_len).collect()
}

/// Clean and validate generated title
fn clean_title(raw_title: &str) -> String {
    let cleaned = raw_title
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .lines()
        .next()
        .unwrap_or("New Chat")
        .to_string();

    if cleaned.len() > 100 {
        format!("{}...", &cleaned[..97])
    } else if cleaned.is_empty() {
        "New Chat".to_string()
    } else {
        cleaned
    }
}

/// Generate a concise title for a conversation based on the first exchange
///
/// # Arguments
/// * `agent` - The agent client to use for title generation
/// * `history` - The conversation history (must have exactly 2 messages)
///
/// # Returns
/// A generated title string
///
/// # Errors
/// Returns an error if:
/// - History doesn't have exactly 2 messages
/// - LLM call fails
pub async fn generate_title(agent: &AgentClient, history: &[Message]) -> Result<String> {
    debug!("generate_title called");

    // Guard: Only generate title if we have exactly 2 messages
    if history.len() != 2 {
        let err_msg = format!(
            "Title generation requires exactly 2 messages, found {}",
            history.len()
        );
        error!("{}", err_msg);
        return Err(anyhow!(err_msg));
    }

    debug!("Message count is 2, proceeding");

    // Extract first exchange
    let user_text = match history.first() {
        Some(Message::User { content, .. }) => content
            .iter()
            .find_map(extract_text_from_user_content)
            .unwrap_or_default(),
        _ => String::new(),
    };

    let assistant_text = match history.get(1) {
        Some(Message::Assistant { content, .. }) => content
            .iter()
            .find_map(extract_text_from_assistant_content)
            .unwrap_or_default(),
        _ => String::new(),
    };

    debug!(
        user_len = user_text.len(),
        assistant_len = assistant_text.len(),
        "Message lengths"
    );

    // Build title generation prompt
    let title_prompt = format!(
        "Generate a concise, descriptive title (3-7 words) for this conversation. \
        Output ONLY the title, no quotes, no explanation.\n\n\
        User: {}\n\nAssistant: {}",
        truncate_text(&user_text, 500),
        truncate_text(&assistant_text, 500)
    );

    // Use agent.prompt() for non-streaming completion
    debug!("Calling LLM for title generation");
    let response_text = match agent {
        AgentClient::Anthropic(agent) => agent.prompt(&title_prompt).await?,
        AgentClient::OpenAI(agent) => agent.prompt(&title_prompt).await?,
        AgentClient::Gemini(agent) => agent.prompt(&title_prompt).await?,
        AgentClient::Mistral(agent) => agent.prompt(&title_prompt).await?,
        AgentClient::Ollama(agent) => agent.prompt(&title_prompt).await?,
    };

    debug!(response = %response_text, "LLM response received");

    // Clean and validate the title
    let title = clean_title(&response_text);

    debug!(cleaned_title = %title, "Title cleaned");

    Ok(title)
}
