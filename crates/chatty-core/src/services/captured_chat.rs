//! A sub-agent's captured conversation, read back as chat entries.
//!
//! The run's terminal status carries every message it took (RC-0), and the
//! swarm trace keeps it. This turns that JSON into the handful of things a
//! chat draws — the prompt, reasoning, text, and tool calls with their
//! results — so the agent transcript sheet can show the run the way the main
//! thread shows a turn. Nothing here crosses a hop: it reads what the trace
//! already holds.

use std::collections::HashMap;

use chatty_fabric::CapturedConversation;
use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, ToolResultContent, UserContent};

use crate::models::message_types::{ToolCallBlock, ToolCallState, ToolSource};

/// One thing the run's chat shows, in order.
#[derive(Debug, Clone)]
pub enum ChatEntry {
    /// What the run was asked (a user message's text).
    Prompt(String),
    /// Reasoning the model wrote.
    Thinking(String),
    /// Text the model wrote.
    Text(String),
    /// A run of tool calls, each with its result when one was captured.
    Tools(Vec<ToolCallBlock>),
}

/// Why a conversation cannot be shown as chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatUnavailable {
    /// It was over the capture cap; only its size came back.
    TooLarge { bytes: u64 },
    /// It was captured but is not a message list this build can read.
    Unreadable,
}

/// `conversation` as chat entries.
pub fn chat_entries(
    conversation: &CapturedConversation,
) -> Result<Vec<ChatEntry>, ChatUnavailable> {
    let messages = match conversation {
        CapturedConversation::Messages { messages } => {
            serde_json::from_value::<Vec<Message>>(messages.clone())
                .map_err(|_| ChatUnavailable::Unreadable)?
        }
        CapturedConversation::TooLarge { bytes } => {
            return Err(ChatUnavailable::TooLarge { bytes: *bytes });
        }
    };

    // Results arrive in a later user message than the call; find them first.
    let mut results: HashMap<String, String> = HashMap::new();
    for message in &messages {
        if let Message::User { content } = message {
            for part in content.iter() {
                if let UserContent::ToolResult(result) = part {
                    let text = result
                        .content
                        .iter()
                        .map(|c| match c {
                            ToolResultContent::Text(t) => t.text.clone(),
                            ToolResultContent::Json { value } => value.to_string(),
                            ToolResultContent::Image(_) => "[image]".to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    results.insert(result.call.to_string(), text);
                }
            }
        }
    }

    let mut entries: Vec<ChatEntry> = Vec::new();
    for message in &messages {
        match message {
            Message::User { content } => {
                let text: Vec<String> = content
                    .iter()
                    .filter_map(|part| match part {
                        UserContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    entries.push(ChatEntry::Prompt(text.join("\n")));
                }
            }
            Message::Assistant { content, .. } => {
                for part in content.iter() {
                    match part {
                        AssistantContent::Reasoning(r) => {
                            let text = r.display_text();
                            if !text.is_empty() {
                                entries.push(ChatEntry::Thinking(text));
                            }
                        }
                        AssistantContent::Text(t) => {
                            if !t.text.trim().is_empty() {
                                entries.push(ChatEntry::Text(t.text.clone()));
                            }
                        }
                        AssistantContent::ToolCall(tc) => {
                            let output = std::iter::once(tc.id.to_string())
                                .chain(tc.provider.as_ref().map(|p| p.call_id.clone()))
                                .find_map(|id| results.get(&id).cloned());
                            let block = ToolCallBlock {
                                id: tc.id.to_string(),
                                tool_name: tc.function.name.clone(),
                                display_name: tc.function.name.clone(),
                                input: tc.function.arguments.to_string(),
                                output,
                                output_preview: None,
                                state: ToolCallState::Success,
                                duration: None,
                                text_before: String::new(),
                                source: ToolSource::default(),
                                execution_engine: None,
                            };
                            match entries.last_mut() {
                                Some(ChatEntry::Tools(run)) => run.push(block),
                                _ => entries.push(ChatEntry::Tools(vec![block])),
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn captured(messages: serde_json::Value) -> CapturedConversation {
        CapturedConversation::Messages { messages }
    }

    #[test]
    fn too_large_and_unreadable_say_why() {
        assert_eq!(
            chat_entries(&CapturedConversation::TooLarge { bytes: 9 }).unwrap_err(),
            ChatUnavailable::TooLarge { bytes: 9 }
        );
        assert_eq!(
            chat_entries(&captured(json!("not a list"))).unwrap_err(),
            ChatUnavailable::Unreadable
        );
    }

    #[test]
    fn a_run_reads_as_prompt_text_and_tools_with_results() {
        let messages: Vec<Message> = vec![
            Message::user("find the answer"),
            Message::Assistant {
                id: None,
                content: vec![
                    AssistantContent::text("looking"),
                    AssistantContent::tool_call("c1", "read_file", json!({"path": "a.md"})),
                    AssistantContent::tool_call("c2", "shell", json!({"cmd": "ls"})),
                ],
            },
            Message::User {
                content: vec![
                    UserContent::tool_result(
                        "c1",
                        "read_file",
                        vec![ToolResultContent::text("file body")],
                    ),
                    UserContent::tool_result("c2", "shell", vec![ToolResultContent::text("a.md")]),
                ],
            },
            Message::assistant("done"),
        ];
        let entries = chat_entries(&captured(serde_json::to_value(&messages).unwrap())).unwrap();
        assert!(matches!(&entries[0], ChatEntry::Prompt(p) if p == "find the answer"));
        assert!(matches!(&entries[1], ChatEntry::Text(t) if t == "looking"));
        let ChatEntry::Tools(tools) = &entries[2] else {
            panic!("tools: {entries:?}");
        };
        assert_eq!(tools.len(), 2, "adjacent calls form one run");
        assert_eq!(tools[0].output.as_deref(), Some("file body"));
        assert_eq!(tools[1].output.as_deref(), Some("a.md"));
        assert!(matches!(&entries[3], ChatEntry::Text(t) if t == "done"));
    }
}
