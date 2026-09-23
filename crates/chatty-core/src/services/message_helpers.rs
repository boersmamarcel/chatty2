use std::collections::HashSet;

use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, ToolCall, ToolCallId, ToolResultContent};
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

/// The ids of the tool calls `message` makes.
pub fn call_ids(message: &Message) -> impl Iterator<Item = &ToolCallId> {
    tool_calls(message).map(|call| &call.id)
}

/// The ids of the tool calls `message` answers.
pub fn result_ids(message: &Message) -> impl Iterator<Item = &ToolCallId> {
    let content = match message {
        Message::User { content } => Some(content.iter()),
        _ => None,
    };
    content.into_iter().flatten().filter_map(|item| match item {
        UserContent::ToolResult(result) => Some(&result.call),
        _ => None,
    })
}

/// The tool calls `message` makes.
fn tool_calls(message: &Message) -> impl Iterator<Item = &ToolCall> {
    let content = match message {
        Message::Assistant { content, .. } => Some(content.iter()),
        _ => None,
    };
    content.into_iter().flatten().filter_map(|item| match item {
        AssistantContent::ToolCall(call) => Some(call),
        _ => None,
    })
}

/// Whether `messages` is a history every OpenAI-compatible provider accepts:
/// every tool result answers a call made before it, and every call is
/// answered. See [`enforce_tool_round_trips`] for `answered_outside` and for
/// what a violation costs.
///
/// Worth checking before repairing: a history that is already intact is sent
/// byte for byte as recorded, which is what keeps the append-only prefix
/// property (AGE-277) — and so the provider's prompt cache — intact.
pub fn tool_round_trips_intact(messages: &[Message], answered_outside: &[ToolCallId]) -> bool {
    let mut called: HashSet<&ToolCallId> = HashSet::new();
    for message in messages {
        if result_ids(message).any(|id| !called.contains(id)) {
            return false;
        }
        called.extend(call_ids(message));
    }
    let answered: HashSet<&ToolCallId> = messages.iter().flat_map(result_ids).collect();
    messages
        .iter()
        .flat_map(call_ids)
        .all(|id| answered.contains(id) || answered_outside.contains(id))
}

/// Repair `messages` into a history every OpenAI-compatible provider accepts:
/// a tool result answering no earlier call is dropped, and an assistant
/// `tool_calls` entry nothing answers gets a placeholder result directly
/// after the message that made it.
///
/// This is the one place that knows the rule, because the same 400 has
/// reached production from both directions: a mid-batch cancellation leaving
/// a call unanswered in the persisted conversation (AGE-485), and the context
/// guard's snip cutting a round-trip in half in the history it builds for one
/// request (AGE-512). Both halves are checked here so the next place that
/// cuts into a history cannot reintroduce either.
///
/// `answered_outside` carries ids answered by a message that is not part of
/// `messages`. rig's completion-call hook passes the request's `prompt`
/// separately from its history, and mid-tool-loop that prompt *is* the result
/// answering the history's last call: synthesizing a second answer for it
/// would be its own 400.
pub fn enforce_tool_round_trips(
    messages: Vec<Message>,
    answered_outside: &[ToolCallId],
) -> Vec<Message> {
    if tool_round_trips_intact(&messages, answered_outside) {
        return messages;
    }
    // In this order: dropping an orphan can leave the call it named
    // unanswered, and that call then needs a placeholder of its own.
    answer_dangling_calls(drop_orphan_results(messages), answered_outside)
}

/// Drop every tool result that answers no call made before it.
fn drop_orphan_results(messages: Vec<Message>) -> Vec<Message> {
    let mut called: HashSet<ToolCallId> = HashSet::new();
    let mut kept_messages: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        let message = match message {
            Message::User { content } => {
                let kept: Vec<UserContent> = content
                    .into_iter()
                    .filter(|item| match item {
                        UserContent::ToolResult(result) if !called.contains(&result.call) => {
                            tracing::warn!(
                                tool_call_id = %result.call,
                                tool_name = %result.name,
                                "History carries a tool result answering no preceding tool call; dropping it"
                            );
                            false
                        }
                        _ => true,
                    })
                    .collect();
                // A message that was nothing but orphaned results is gone.
                if kept.is_empty() {
                    continue;
                }
                Message::User { content: kept }
            }
            other => other,
        };
        called.extend(call_ids(&message).cloned());
        kept_messages.push(message);
    }
    kept_messages
}

/// Give every unanswered tool call a placeholder result, behind the batch it
/// belongs to so the results that did arrive keep their place.
fn answer_dangling_calls(messages: Vec<Message>, answered_outside: &[ToolCallId]) -> Vec<Message> {
    let answered: HashSet<ToolCallId> = messages
        .iter()
        .flat_map(result_ids)
        .chain(answered_outside)
        .cloned()
        .collect();

    let mut repaired: Vec<Message> = Vec::with_capacity(messages.len());
    let mut pending: Vec<Message> = Vec::new();
    for message in messages {
        // A batch ends at the first message that is not a tool result.
        if !is_tool_result_message(&message) {
            repaired.append(&mut pending);
        }
        pending.extend(
            tool_calls(&message)
                .filter(|call| !answered.contains(&call.id))
                .map(placeholder_result)
                .collect::<Vec<_>>(),
        );
        repaired.push(message);
    }
    repaired.append(&mut pending);
    repaired
}

/// A stand-in for a result that never arrived.
fn placeholder_result(call: &ToolCall) -> Message {
    tracing::warn!(
        tool_call_id = %call.id,
        tool_name = %call.function.name,
        "History carries a tool call nothing answers; synthesizing a placeholder result"
    );
    Message::User {
        content: vec![UserContent::tool_result_for(
            call.id.clone(),
            call.provider.clone(),
            call.function.name.clone(),
            vec![ToolResultContent::text(
                "Cancelled: the turn ended before this tool call's result arrived.",
            )],
        )],
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

    /// One assistant message with `n` parallel tool calls, `call-1`..`call-n`
    /// — the shape a provider that supports parallel tool calls sends back
    /// in one turn, and the shape AGE-485's malformed-history bug is about.
    fn parallel_tool_calls(n: usize) -> Message {
        Message::Assistant {
            id: None,
            content: (1..=n)
                .map(|i| {
                    AssistantContent::tool_call(
                        format!("call-{i}"),
                        "read_file",
                        serde_json::json!({ "path": format!("/tmp/{i}") }),
                    )
                })
                .collect(),
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

    /// AGE-485: a batch cut short mid-way (loop-guard pivot, or a user
    /// cancel) leaves later calls in the same assistant message unanswered.
    /// The repair must synthesize a placeholder for every missing id and
    /// leave the answered ones untouched.
    #[test]
    fn a_call_nothing_answers_gets_a_placeholder_behind_the_batch() {
        let messages = vec![
            Message::user("read three files"),
            parallel_tool_calls(3),
            Message::tool_result("call-1", "read_file", "a"),
            // call-2 and call-3 never got a result before the turn ended.
        ];

        let repaired = enforce_tool_round_trips(messages, &[]);

        assert!(tool_round_trips_intact(&repaired, &[]));
        assert_eq!(repaired.len(), 5);
        let answered: Vec<&str> = repaired
            .iter()
            .filter_map(|m| match m {
                Message::User { content } => content.iter().find_map(|c| match c {
                    UserContent::ToolResult(r) => Some(r.call.as_str()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(answered, vec!["call-1", "call-2", "call-3"]);
    }

    /// A fully-answered batch is left exactly as it was — no placeholders,
    /// same message count.
    #[test]
    fn an_intact_history_is_returned_unchanged() {
        let messages = vec![
            Message::user("read it"),
            tool_call(),
            Message::tool_result("call-1", "read_file", "a"),
            Message::assistant("done"),
        ];

        assert!(tool_round_trips_intact(&messages, &[]));
        let repaired = enforce_tool_round_trips(messages.clone(), &[]);
        assert_eq!(repaired, messages);
    }

    /// AGE-512's half: a cut that drops the call but keeps its answer. The
    /// orphan is dropped — it answers a question the model never asked.
    #[test]
    fn a_result_answering_no_call_is_dropped() {
        let messages = vec![
            Message::user("[CONTEXT SHAPER: 4 messages snipped]"),
            Message::tool_result("call-1", "read_file", "a"),
            Message::assistant("done"),
        ];

        assert!(!tool_round_trips_intact(&messages, &[]));
        let repaired = enforce_tool_round_trips(messages, &[]);

        assert!(tool_round_trips_intact(&repaired, &[]));
        assert_eq!(repaired.len(), 2, "the orphaned result's message is gone");
        assert!(matches!(&repaired[1], Message::Assistant { .. }));
    }

    /// An orphaned result sharing its message with real content costs only
    /// the result: the text the user wrote stays.
    #[test]
    fn an_orphaned_result_beside_text_costs_only_the_result() {
        let messages = vec![Message::User {
            content: vec![
                UserContent::text("and this too"),
                UserContent::tool_result_for(
                    ToolCallId::new("call-1").unwrap(),
                    None,
                    "read_file".to_string(),
                    vec![ToolResultContent::text("a")],
                ),
            ],
        }];

        let repaired = enforce_tool_round_trips(messages, &[]);

        assert_eq!(repaired.len(), 1);
        let Message::User { content } = &repaired[0] else {
            panic!("expected a user message");
        };
        assert_eq!(content.len(), 1);
        assert!(matches!(content[0], UserContent::Text(_)));
    }

    /// The trap the hook has to avoid: rig hands the completion-call hook the
    /// prompt separately from the history, and mid-tool-loop that prompt is
    /// the result answering the history's last call. Synthesizing a second
    /// answer for it would be its own 400.
    #[test]
    fn a_call_the_prompt_answers_is_left_alone() {
        let messages = vec![Message::user("read it"), tool_call()];
        let answered_by_prompt = [ToolCallId::new("call-1").unwrap()];

        assert!(
            !tool_round_trips_intact(&messages, &[]),
            "without the prompt the call looks unanswered"
        );
        assert!(tool_round_trips_intact(&messages, &answered_by_prompt));
        assert_eq!(
            enforce_tool_round_trips(messages.clone(), &answered_by_prompt),
            messages
        );
    }

    /// A result that arrives before the call it names is an orphan too —
    /// order, not mere presence, is what the providers check.
    #[test]
    fn a_result_before_its_call_is_an_orphan() {
        let messages = vec![
            Message::tool_result("call-1", "read_file", "a"),
            tool_call(),
        ];

        assert!(!tool_round_trips_intact(&messages, &[]));
        let repaired = enforce_tool_round_trips(messages, &[]);

        // The early result is dropped, and the call it should have answered
        // is given a placeholder behind it.
        assert!(tool_round_trips_intact(&repaired, &[]));
        assert_eq!(repaired.len(), 2);
        assert!(is_tool_call_message(&repaired[0]));
        assert!(is_tool_result_message(&repaired[1]));
    }
}
