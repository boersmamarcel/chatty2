use rig_core::completion::Message;
use rig_core::completion::message::AssistantContent;
use rig_core::message::UserContent;
use tracing::info;

/// Text portions of user contents, filtering out non-text content (images, PDFs).
fn user_text_parts(contents: &[UserContent]) -> impl Iterator<Item = &str> {
    contents.iter().filter_map(|c| match c {
        UserContent::Text(t) => Some(t.text.as_str()),
        _ => None,
    })
}

/// Extract the text portion of user contents for memory query, joining fragments with a space.
///
/// Shared between GPUI (which had this in token_budget/manager.rs) and TUI.
pub fn extract_user_text(contents: &[UserContent]) -> String {
    user_text_parts(contents).collect::<Vec<_>>().join(" ")
}

/// Extract the text portion of user contents, joining fragments with a newline.
///
/// Used where paragraph breaks in the original text should be preserved (e.g. exported
/// training data), unlike `extract_user_text`'s single-line join.
pub fn extract_user_text_lines(contents: &[UserContent]) -> String {
    user_text_parts(contents).collect::<Vec<_>>().join("\n")
}

/// Whether `message` is an assistant message that calls tools — the first half
/// of a tool round-trip, persisted with the turn since AGE-247.
pub fn is_tool_call_message(message: &Message) -> bool {
    matches!(
        message,
        Message::Assistant { content, .. }
            if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)))
    )
}

/// Whether `message` is a user message made only of tool results — the
/// second half of a tool round-trip.
pub fn is_tool_result_message(message: &Message) -> bool {
    matches!(
        message,
        Message::User { content }
            if !content.is_empty()
                && content.iter().all(|c| matches!(c, UserContent::ToolResult(_)))
    )
}

/// Whether `message` belongs to a tool round-trip rather than being a human
/// or assistant text turn. Readers that render, export or count turns skip
/// these; the model sees them.
pub fn is_tool_message(message: &Message) -> bool {
    is_tool_call_message(message) || is_tool_result_message(message)
}

/// Whether `history[index]` is one half of a persisted tool round-trip
/// (AGE-247): a tool-result message, or a tool-call message whose result
/// follows it. An assistant message that carries tool calls with no result
/// behind it is not one — that is a single step with its calls and
/// observations attached, the shape the exporters read from the trace.
pub fn is_persisted_tool_round_trip(history: &[Message], index: usize) -> bool {
    let Some(message) = history.get(index) else {
        return false;
    };
    is_tool_result_message(message)
        || (is_tool_call_message(message)
            && history.get(index + 1).is_some_and(is_tool_result_message))
}

/// Number of completed exchanges: a user text message answered by an
/// assistant text message, whatever tool round-trips sit between them.
///
/// The "first exchange" checks that used to count messages read this instead,
/// since a turn with tool calls persists more than two messages.
pub fn exchange_count<'a>(messages: impl IntoIterator<Item = &'a Message>) -> usize {
    let mut awaiting_answer = false;
    let mut exchanges = 0;
    for message in messages {
        if is_tool_message(message) {
            continue;
        }
        match message {
            Message::User { .. } => awaiting_answer = true,
            Message::Assistant { .. } if awaiting_answer => {
                exchanges += 1;
                awaiting_answer = false;
            }
            _ => {}
        }
    }
    exchanges
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

    #[test]
    fn extract_user_text_lines_joins_with_newline() {
        let contents = vec![UserContent::text("hello"), UserContent::text("world")];
        assert_eq!(extract_user_text_lines(&contents), "hello\nworld");
    }

    fn tool_call() -> Message {
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::text("Let me look."),
                AssistantContent::tool_call("call-1", "read_file", serde_json::json!({})),
            ],
        }
    }

    #[test]
    fn tool_messages_are_recognised_on_both_sides_of_the_round_trip() {
        assert!(is_tool_call_message(&tool_call()));
        assert!(is_tool_result_message(&Message::tool_result(
            "call-1",
            "read_file",
            "contents"
        )));

        assert!(!is_tool_message(&Message::user("hi")));
        assert!(!is_tool_message(&Message::assistant("hello")));
        assert!(!is_tool_message(&Message::User {
            content: Vec::new()
        }));
    }

    #[test]
    fn persisted_round_trips_need_the_result_behind_the_call() {
        let history = vec![
            Message::user("read it"),
            tool_call(),
            Message::tool_result("call-1", "read_file", "a"),
            Message::assistant("done"),
            // A call with nothing after it: a single-step shape, not a round-trip.
            tool_call(),
        ];
        assert!(!is_persisted_tool_round_trip(&history, 0));
        assert!(is_persisted_tool_round_trip(&history, 1));
        assert!(is_persisted_tool_round_trip(&history, 2));
        assert!(!is_persisted_tool_round_trip(&history, 3));
        assert!(!is_persisted_tool_round_trip(&history, 4));
        assert!(!is_persisted_tool_round_trip(&history, 5));
    }

    #[test]
    fn exchange_count_ignores_tool_round_trips_and_unanswered_prompts() {
        assert_eq!(exchange_count(&[]), 0);
        assert_eq!(exchange_count(&[Message::user("hi")]), 0);
        assert_eq!(
            exchange_count(&[Message::user("hi"), Message::assistant("hello")]),
            1
        );
        // A first turn with two tool calls persists five messages, one exchange.
        assert_eq!(
            exchange_count(&[
                Message::user("read it"),
                tool_call(),
                Message::tool_result("call-1", "read_file", "a"),
                tool_call(),
                Message::tool_result("call-1", "read_file", "b"),
                Message::assistant("done"),
            ]),
            1
        );
        assert_eq!(
            exchange_count(&[
                Message::user("one"),
                Message::assistant("1"),
                Message::user("two"),
                Message::assistant("2"),
                Message::user("three"),
            ]),
            2
        );
    }
}
