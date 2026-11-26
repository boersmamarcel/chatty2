use anyhow::{Result, anyhow};
use rig_core::completion::Message;
use rig_core::completion::message::AssistantContent;
use rig_core::message::UserContent;
use tracing::{debug, error};

use crate::factories::AgentClient;
use crate::services::message_helpers::{
    exchange_count, is_tool_call_message, is_tool_result_message,
};

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

/// The reasoning tags some models put ahead of their answer.
const REASONING_TAGS: [&str; 2] = ["thinking", "think"];

/// `text` without its reasoning: each `<thinking>…</thinking>` or
/// `<think>…</think>` block goes, an unclosed one to the end, and so does a
/// stray closing tag. Titles were "<thinking>" (AGE-2129).
fn strip_reasoning(text: &str) -> String {
    let mut rest = text.to_string();
    for tag in REASONING_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = rest.find(&open) {
            match rest[start..].find(&close) {
                Some(end) => rest.replace_range(start..start + end + close.len(), "\n"),
                None => rest.truncate(start),
            }
        }
        rest = rest.replace(&close, "\n");
    }
    rest
}

/// Clean and validate generated title
///
/// Titles longer than 100 characters keep the first 97 and get `...`.
fn clean_title(raw_title: &str) -> String {
    let without_reasoning = strip_reasoning(raw_title);
    let cleaned = without_reasoning
        .lines()
        .map(|line| line.trim().trim_matches('"').trim_matches('\'').trim())
        .find(|line| !line.is_empty())
        .unwrap_or("New Chat")
        .to_string();

    if cleaned.chars().count() > 100 {
        let head: String = cleaned.chars().take(97).collect();
        format!("{head}...")
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
/// * `history` - The conversation history (must hold exactly one exchange)
///
/// # Returns
/// A generated title string
///
/// # Errors
/// Returns an error if:
/// - History doesn't hold exactly one exchange
/// - LLM call fails
pub async fn generate_title(agent: &AgentClient, history: &[Message]) -> Result<String> {
    debug!("generate_title called");

    // Guard: only the first exchange gets a title. Exchanges, not messages:
    // a turn with tool calls persists its tool round-trips too (AGE-247).
    let exchanges = exchange_count(history);
    if exchanges != 1 {
        let err_msg = format!(
            "Title generation requires exactly one exchange, found {} in {} messages",
            exchanges,
            history.len()
        );
        error!("{}", err_msg);
        return Err(anyhow!(err_msg));
    }

    debug!("Exchange count is 1, proceeding");

    // Extract first exchange: the first user text and the first assistant text.
    let user_text = history
        .iter()
        .find_map(|message| match message {
            Message::User { content } if !is_tool_result_message(message) => {
                content.iter().find_map(extract_text_from_user_content)
            }
            _ => None,
        })
        .unwrap_or_default();

    let assistant_text = history
        .iter()
        .find_map(|message| match message {
            Message::Assistant(rig_core::message::AssistantMessage { content, .. })
                if !is_tool_call_message(message) =>
            {
                content.iter().find_map(extract_text_from_assistant_content)
            }
            _ => None,
        })
        .unwrap_or_default();

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
    let response_text = agent.prompt(&title_prompt).await?;

    debug!(response = %response_text, "LLM response received");

    // Clean and validate the title
    let title = clean_title(&response_text);

    debug!(cleaned_title = %title, "Title cleaned");

    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::clean_title;

    #[test]
    fn a_long_title_with_multibyte_characters_is_cut_by_characters() {
        assert_eq!(clean_title(&"é".repeat(120)), "é".repeat(97) + "...");
    }

    #[test]
    fn a_title_of_60_multibyte_characters_is_kept_whole() {
        assert_eq!(clean_title(&"é".repeat(60)), "é".repeat(60));
    }

    #[test]
    fn an_ascii_title_over_100_characters_keeps_97_and_an_ellipsis() {
        assert_eq!(clean_title(&"a".repeat(150)), "a".repeat(97) + "...");
    }

    #[test]
    fn quotes_extra_lines_and_blanks() {
        assert_eq!(clean_title("\"Trip to Paris\""), "Trip to Paris");
        assert_eq!(clean_title("Trip to Paris\nsecond line"), "Trip to Paris");
        assert_eq!(clean_title("   "), "New Chat");
    }

    #[test]
    fn reasoning_never_becomes_the_title() {
        assert_eq!(
            clean_title("<thinking>The user asks about trips.</thinking>\nTrip to Rome"),
            "Trip to Rome"
        );
        assert_eq!(
            clean_title("<think>\nhmm\n</think>\n\n\"Budget plan\""),
            "Budget plan"
        );
        assert_eq!(clean_title("<thinking>"), "New Chat");
        assert_eq!(clean_title("Weekly report</thinking>"), "Weekly report");
    }
}
