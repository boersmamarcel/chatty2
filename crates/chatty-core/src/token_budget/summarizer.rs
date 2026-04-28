use anyhow::Result;
use rig::OneOrMany;
use rig::completion::Message;
use rig::completion::message::{AssistantContent, Text};
use rig::message::UserContent;
use tracing::{debug, info};

use crate::factories::AgentClient;

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of a summarization pass.
#[derive(Debug, Clone)]
pub struct SummarizationResult {
    /// The replacement history (summary message + tail of original history).
    pub new_history: Vec<Message>,
    /// Number of original messages that were collapsed into the summary.
    pub messages_summarized: usize,
    /// Approximate number of tokens freed by the operation (old_count - new_count estimate).
    /// Computed as `(chars_before - chars_after) / 4` — a rough approximation, not a BPE count.
    pub estimated_tokens_freed: usize,
}

/// Summarize the oldest half of a conversation history.
///
/// Takes the first `N/2` messages (rounded down), sends them to the model with a
/// compression prompt, and returns a new history where those messages are replaced
/// by a single assistant-style summary message. The second half of the original
/// history is preserved verbatim so no recent context is lost.
///
/// # When to call
/// Triggered either automatically (when `TokenTrackingSettings.auto_summarize` is
/// true and a `CriticalPressure` event fires) or manually (user clicks the
/// "Summarize" button in the context bar popover).
///
/// # Errors
/// Returns an error if:
/// - The history has fewer than 4 messages (nothing worth summarizing)
/// - The LLM call fails (network error, API error, etc.)
///
/// The original history is **never mutated** — errors leave the conversation
/// state unchanged.
pub async fn summarize_oldest_half(
    agent: &AgentClient,
    history: &[Message],
) -> Result<SummarizationResult> {
    if history.len() < 4 {
        return Err(anyhow::anyhow!(
            "History too short to summarize ({} messages; need ≥ 4)",
            history.len()
        ));
    }

    let midpoint = history.len() / 2;
    let to_summarize = &history[..midpoint];
    let to_keep = &history[midpoint..];

    debug!(
        total_messages = history.len(),
        summarizing = midpoint,
        keeping = to_keep.len(),
        "Starting conversation summarization"
    );

    // Measure approximate token cost of the section being summarized so we can
    // report `estimated_tokens_freed` after the summary is built.
    let chars_before: usize = to_summarize.iter().map(message_char_len).sum();

    // Build the transcript to compress
    let transcript = build_transcript(to_summarize);

    let prompt = format!(
        "The following is the beginning of a conversation that needs to be compressed \
         to free up context space. Summarize it into a dense set of bullet points that \
         preserves ALL of the following:\n\
         \n\
         - Key decisions made and the reasoning behind them\n\
         - All code written, modified, or discussed (include file paths and function names)\n\
         - Commands run and their outcomes\n\
         - Files created, edited, or deleted\n\
         - Unresolved issues, open questions, and next steps\n\
         - Any important facts the assistant was told (e.g. project structure, constraints)\n\
         \n\
         Output ONLY the bullet-point summary. Do not include any preamble or explanation.\n\
         \n\
         --- CONVERSATION TO SUMMARIZE ---\n\
         {transcript}"
    );

    info!(
        transcript_chars = transcript.len(),
        "Calling LLM to compress conversation history"
    );

    let summary_text = call_agent(agent, &prompt).await?;

    let summary_chars = summary_text.len();
    let estimated_tokens_freed = chars_before.saturating_sub(summary_chars) / 4;

    info!(
        chars_before,
        chars_after = summary_chars,
        estimated_tokens_freed,
        messages_summarized = midpoint,
        "Conversation summarization complete"
    );

    // Wrap the summary as a User message prefixed with a clear marker so the LLM
    // knows it is reading a compressed history, not a live user turn.
    let summary_message = Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: format!(
                "[CONVERSATION SUMMARY — {midpoint} messages compressed]\n\n{summary_text}"
            ),
        })),
    };

    let mut new_history = Vec::with_capacity(1 + to_keep.len());
    new_history.push(summary_message);
    new_history.extend_from_slice(to_keep);

    Ok(SummarizationResult {
        new_history,
        messages_summarized: midpoint,
        estimated_tokens_freed,
    })
}

/// Summarize using a specific model ID rather than the active conversation's model.
///
/// Useful when `TokenTrackingSettings.summarization_model_id` is set to a cheaper/faster
/// model (e.g. `"qwen3:8b"` locally, or `"gpt-4o-mini"` for cloud users).
///
/// Currently unimplemented — the plumbing for looking up a secondary agent by model ID
/// requires access to provider configs and the MCP service, which is easier to wire
/// in `app_controller.rs`. This stub exists so the settings field has a clear call site.
///
/// # Errors
/// Always returns an error in the current version. Replace with a real implementation
/// when the secondary-model path is wired up.
#[allow(dead_code)]
pub async fn summarize_with_model(
    _model_id: &str,
    _history: &[Message],
) -> Result<SummarizationResult> {
    anyhow::bail!(
        "summarize_with_model is not yet implemented. \
         Use summarize_oldest_half() with the conversation's own agent for now."
    )
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Render a slice of messages as a plain-text transcript suitable for inclusion
/// in a summarization prompt.
///
/// Format:
/// ```text
/// User: <text content>
/// Assistant: <text content>
/// ```
/// Non-text content (images, PDFs, tool calls, tool results) is represented
/// by a placeholder so the LLM understands what was present without receiving
/// binary data.
fn build_transcript(messages: &[Message]) -> String {
    let mut parts = Vec::with_capacity(messages.len());

    for message in messages {
        match message {
            Message::User { content } => {
                let text = extract_user_text(content);
                if !text.is_empty() {
                    parts.push(format!("User: {}", truncate(&text, 4_000)));
                } else {
                    parts.push("User: [non-text content]".to_string());
                }
            }
            Message::Assistant { content, .. } => {
                let text = extract_assistant_text(content);
                if !text.is_empty() {
                    parts.push(format!("Assistant: {}", truncate(&text, 4_000)));
                } else {
                    parts.push("Assistant: [tool calls / non-text response]".to_string());
                }
            }
            Message::System { content } => {
                if !content.is_empty() {
                    parts.push(format!("System: {}", truncate(content, 4_000)));
                }
            }
        }
    }

    parts.join("\n\n")
}

/// Extract plain text from `UserContent`, joining multiple text parts with a space.
fn extract_user_text(content: &OneOrMany<UserContent>) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract plain text from `AssistantContent`, joining multiple text parts.
fn extract_assistant_text(content: &OneOrMany<AssistantContent>) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Approximate character length of a `Message` (for token-freed estimation).
fn message_char_len(message: &Message) -> usize {
    match message {
        Message::User { content } => extract_user_text(content).len(),
        Message::Assistant { content, .. } => extract_assistant_text(content).len(),
        Message::System { content } => content.len(),
    }
}

/// Truncate a string to at most `max_chars` characters, appending `…` if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

/// Dispatch a non-streaming prompt call to whichever provider the agent wraps.
async fn call_agent(agent: &AgentClient, prompt: &str) -> Result<String> {
    agent.prompt(prompt).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        let result = truncate("hello world", 5);
        assert!(result.ends_with('…'), "expected ellipsis, got: {result}");
        assert!(result.len() <= 10); // 5 chars + ellipsis byte(s)
    }

    #[test]
    fn build_transcript_formats_user_and_assistant() {
        let messages = vec![
            user_msg("What is Rust?"),
            assistant_msg("Rust is a systems programming language."),
        ];
        let transcript = build_transcript(&messages);
        assert!(transcript.contains("User: What is Rust?"));
        assert!(transcript.contains("Assistant: Rust is a systems programming language."));
    }

    #[test]
    fn build_transcript_empty_user_content_shows_placeholder() {
        // An empty OneOrMany<UserContent> produces no text items
        let message = Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: String::new(),
            })),
        };
        let transcript = build_transcript(&[message]);
        // Empty text still shows placeholder because text.is_empty() == true
        assert!(transcript.contains("[non-text content]"));
    }

    #[test]
    fn extract_user_text_collects_multiple_text_parts() {
        let content: OneOrMany<UserContent> = OneOrMany::many(vec![
            UserContent::Text(Text {
                text: "Hello".to_string(),
            }),
            UserContent::Text(Text {
                text: "world".to_string(),
            }),
        ])
        .unwrap();
        assert_eq!(extract_user_text(&content), "Hello world");
    }

    #[test]
    fn message_char_len_nonzero_for_nonempty_message() {
        let msg = user_msg("This is a test message.");
        assert!(message_char_len(&msg) > 0);
    }

    #[test]
    fn message_char_len_zero_for_empty_message() {
        let msg = user_msg("");
        assert_eq!(message_char_len(&msg), 0);
    }
}
