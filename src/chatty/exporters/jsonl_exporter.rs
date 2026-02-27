use std::collections::HashMap;

use anyhow::{Context, Result};
use rig::completion::Message;
use rig::completion::message::{AssistantContent, UserContent};

use crate::chatty::models::conversation::RegenerationRecord;
use crate::chatty::repositories::ConversationData;
use crate::chatty::views::message_types::{SystemTrace, TraceItem};
use crate::settings::models::models_store::ModelConfig;

/// Configuration options for SFT JSONL export
#[derive(Clone, Debug)]
pub(crate) struct SftExportOptions {
    /// If true and ModelConfig.preamble is non-empty, prepend a system message
    pub include_system_prompt: bool,
    /// If true, include tool_call and tool messages in ChatML format
    pub include_tool_calls: bool,
    /// Skip conversations with fewer messages than this threshold
    pub min_messages: usize,
}

impl Default for SftExportOptions {
    fn default() -> Self {
        Self {
            include_system_prompt: true,
            include_tool_calls: false,
            min_messages: 2,
        }
    }
}

/// Convert a persisted conversation into SFT (Supervised Fine-Tuning) JSONL format.
///
/// Returns `Ok(None)` if the conversation is filtered out (too few messages, etc.)
/// or `Ok(Some(value))` with a ChatML-format JSON object.
///
/// This is a pure function with no side effects.
///
/// Phases:
/// 1. Deserialize parallel arrays from ConversationData
/// 2. Apply min_messages filter
/// 3. Build ChatML messages array (text-only, stripping multimodal content)
/// 4. Return JSON object with messages and _conversation_id
pub fn conversation_to_sft_jsonl(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
    options: &SftExportOptions,
) -> Result<Option<serde_json::Value>> {
    // PHASE 1: Deserialize parallel arrays
    let history: Vec<Message> = serde_json::from_str(&conversation.message_history)
        .context("Failed to parse message_history")?;
    let traces: Vec<Option<serde_json::Value>> =
        serde_json::from_str(&conversation.system_traces).unwrap_or_default();

    // PHASE 2: Apply filters
    if history.len() < options.min_messages {
        return Ok(None);
    }

    // PHASE 3: Build ChatML messages array
    let mut messages: Vec<serde_json::Value> = Vec::new();

    // Optionally prepend system prompt
    if options.include_system_prompt
        && let Some(cfg) = model_config
        && !cfg.preamble.is_empty()
    {
        messages.push(serde_json::json!({
            "role": "system",
            "content": cfg.preamble
        }));
    }

    for (idx, message) in history.iter().enumerate() {
        match message {
            Message::User { content } => {
                let text = extract_user_text(content);
                if !text.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": text
                    }));
                }
            }
            Message::Assistant { content, .. } => {
                if options.include_tool_calls {
                    // Collect tool calls and text separately
                    let tool_calls: Vec<serde_json::Value> = content
                        .iter()
                        .filter_map(|ac| match ac {
                            AssistantContent::ToolCall(tc) => {
                                let id =
                                    tc.call_id.clone().unwrap_or_else(|| tc.id.clone());
                                Some(serde_json::json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.function.name,
                                        "arguments": serde_json::to_string(&tc.function.arguments).unwrap_or_default()
                                    }
                                }))
                            }
                            _ => None,
                        })
                        .collect();

                    let text = extract_assistant_text(content);

                    // Emit assistant message with tool_calls if present
                    if !tool_calls.is_empty() {
                        let mut msg = serde_json::json!({
                            "role": "assistant",
                            "tool_calls": tool_calls
                        });
                        if !text.is_empty() {
                            msg["content"] = serde_json::Value::String(text.clone());
                        }
                        messages.push(msg);

                        // Emit tool result messages from trace data
                        let trace_outputs = traces
                            .get(idx)
                            .cloned()
                            .flatten()
                            .map(parse_trace_outputs)
                            .unwrap_or_default();

                        for ac in content.iter() {
                            if let AssistantContent::ToolCall(tc) = ac {
                                let call_id = tc.call_id.clone().unwrap_or_else(|| tc.id.clone());
                                let output = trace_outputs.get(&tc.id).cloned().unwrap_or_default();
                                messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": call_id,
                                    "content": output
                                }));
                            }
                        }
                    } else if !text.is_empty() {
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text
                        }));
                    }
                } else {
                    let text = extract_assistant_text(content);
                    if !text.is_empty() {
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text
                        }));
                    }
                }
            }
        }
    }

    // PHASE 4: Return result
    Ok(Some(serde_json::json!({
        "messages": messages,
        "_conversation_id": conversation.id
    })))
}

/// Convert a persisted conversation into DPO (Direct Preference Optimization) JSONL lines.
///
/// Produces one preference pair per `RegenerationRecord`. Returns an empty vec
/// if no regeneration records exist.
///
/// This is a pure function with no side effects.
///
/// Phases:
/// 1. Deserialize message history and regeneration records
/// 2. For each record, build prompt prefix + chosen/rejected pair
pub fn conversation_to_dpo_jsonl(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
) -> Result<Vec<serde_json::Value>> {
    // PHASE 1: Deserialize
    let history: Vec<Message> = serde_json::from_str(&conversation.message_history)
        .context("Failed to parse message_history")?;
    let regeneration_records: Vec<RegenerationRecord> =
        serde_json::from_str(&conversation.regeneration_records).unwrap_or_default();

    if regeneration_records.is_empty() {
        return Ok(Vec::new());
    }

    // PHASE 2: Build preference pairs
    let mut results = Vec::with_capacity(regeneration_records.len());

    for record in &regeneration_records {
        // Build prompt: all messages before the regenerated message
        let prompt = messages_to_chatml_prefix(&history, record.message_index, model_config);

        // Chosen: the current (replacement) text at history[message_index]
        let chosen = history
            .get(record.message_index)
            .map(|msg| match msg {
                Message::Assistant { content, .. } => extract_assistant_text(content),
                Message::User { content } => extract_user_text(content),
            })
            .unwrap_or_default();

        // Rejected: the original text before regeneration
        let rejected = &record.original_text;

        results.push(serde_json::json!({
            "prompt": prompt,
            "chosen": chosen,
            "rejected": rejected,
            "_conversation_id": conversation.id
        }));
    }

    Ok(results)
}

/// Extract text-only content from user message, stripping images and documents.
fn extract_user_text(content: &rig::OneOrMany<UserContent>) -> String {
    content
        .iter()
        .filter_map(|uc| match uc {
            UserContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract text-only content from assistant message, stripping tool calls.
fn extract_assistant_text(content: &rig::OneOrMany<AssistantContent>) -> String {
    content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Convert a slice of message history into ChatML-format JSON array.
fn messages_to_chatml_prefix(
    history: &[Message],
    up_to: usize,
    model_config: Option<&ModelConfig>,
) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();

    // Optionally prepend system prompt
    if let Some(cfg) = model_config
        && !cfg.preamble.is_empty()
    {
        messages.push(serde_json::json!({
            "role": "system",
            "content": cfg.preamble
        }));
    }

    for msg in history.iter().take(up_to) {
        match msg {
            Message::User { content } => {
                let text = extract_user_text(content);
                if !text.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": text
                    }));
                }
            }
            Message::Assistant { content, .. } => {
                let text = extract_assistant_text(content);
                if !text.is_empty() {
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": text
                    }));
                }
            }
        }
    }

    messages
}

/// Parse trace JSON to extract tool call outputs keyed by tool call ID.
fn parse_trace_outputs(trace_json: serde_json::Value) -> HashMap<String, String> {
    let mut outputs = HashMap::new();

    let trace: SystemTrace = match serde_json::from_value(trace_json) {
        Ok(t) => t,
        Err(_) => return outputs,
    };

    for item in &trace.items {
        if let TraceItem::ToolCall(tc) = item
            && let Some(output) = &tc.output
        {
            outputs.insert(tc.id.clone(), output.clone());
        }
    }

    outputs
}

/// Append JSONL lines to a file, replacing any existing lines with the same `_conversation_id`.
///
/// Strategy:
/// 1. Read existing file (if any)
/// 2. Filter out lines matching the conversation_id
/// 3. Append new lines
/// 4. Write atomically (temp file + rename)
pub(crate) async fn append_jsonl_with_dedup(
    path: &std::path::Path,
    new_lines: &[serde_json::Value],
    conversation_id: &str,
) -> anyhow::Result<()> {
    // Read existing content
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    // Filter out old lines for this conversation_id
    let mut output_lines: Vec<String> = existing
        .lines()
        .filter(|line| {
            if line.trim().is_empty() {
                return false;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(val) => {
                    val.get("_conversation_id").and_then(|v| v.as_str()) != Some(conversation_id)
                }
                Err(_) => true, // Keep unparseable lines
            }
        })
        .map(|s| s.to_string())
        .collect();

    // Append new lines
    for val in new_lines {
        output_lines.push(serde_json::to_string(val)?);
    }

    // Write atomically
    let temp_path = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    let content = if output_lines.is_empty() {
        String::new()
    } else {
        output_lines.join("\n") + "\n"
    };
    tokio::fs::write(&temp_path, &content).await?;
    tokio::fs::rename(&temp_path, path).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::models::conversation::{MessageFeedback, RegenerationRecord};
    use crate::chatty::views::message_types::{ToolCallBlock, ToolCallState};
    use crate::settings::models::providers_store::ProviderType;
    use rig::OneOrMany;
    use rig::completion::message::Text;
    use std::collections::HashMap;

    fn make_conversation_data(
        id: &str,
        model_id: &str,
        history: Vec<Message>,
        traces: Vec<Option<serde_json::Value>>,
        feedback: Vec<Option<MessageFeedback>>,
        regeneration_records: Vec<RegenerationRecord>,
    ) -> ConversationData {
        ConversationData {
            id: id.to_string(),
            title: "Test".to_string(),
            model_id: model_id.to_string(),
            message_history: serde_json::to_string(&history).unwrap(),
            system_traces: serde_json::to_string(&traces).unwrap(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            message_feedback: serde_json::to_string(&feedback).unwrap(),
            regeneration_records: serde_json::to_string(&regeneration_records).unwrap(),
            created_at: 1700000000,
            updated_at: 1700000100,
        }
    }

    fn make_model_config(preamble: &str) -> ModelConfig {
        ModelConfig {
            id: "test-id".to_string(),
            name: "Test Model".to_string(),
            provider_type: ProviderType::Anthropic,
            model_identifier: "claude-sonnet-4-20250514".to_string(),
            temperature: 0.7,
            preamble: preamble.to_string(),
            max_tokens: None,
            top_p: None,
            extra_params: HashMap::new(),
            cost_per_million_input_tokens: None,
            cost_per_million_output_tokens: None,
            supports_images: true,
            supports_pdf: true,
            supports_temperature: true,
        }
    }

    fn user_message(text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    fn assistant_message(text: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: text.to_string(),
            })),
        }
    }

    // ── SFT tests ─────────────────────────────────────────────────────

    #[test]
    fn sft_basic_conversation() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hello!"), assistant_message("Hi there!")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let opts = SftExportOptions::default();
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        let val = result.unwrap();

        assert_eq!(val["_conversation_id"], "conv-1");
        let messages = val["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello!");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "Hi there!");
    }

    #[test]
    fn sft_with_system_prompt() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hello!"), assistant_message("Hi!")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let cfg = make_model_config("You are a helpful assistant.");
        let opts = SftExportOptions {
            include_system_prompt: true,
            ..Default::default()
        };
        let result = conversation_to_sft_jsonl(&conv, Some(&cfg), &opts).unwrap();
        let val = result.unwrap();

        let messages = val["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
    }

    #[test]
    fn sft_without_system_prompt_when_disabled() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hello!"), assistant_message("Hi!")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let cfg = make_model_config("You are a helpful assistant.");
        let opts = SftExportOptions {
            include_system_prompt: false,
            ..Default::default()
        };
        let result = conversation_to_sft_jsonl(&conv, Some(&cfg), &opts).unwrap();
        let val = result.unwrap();

        let messages = val["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn sft_no_system_prompt_when_preamble_empty() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hello!"), assistant_message("Hi!")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let cfg = make_model_config("");
        let opts = SftExportOptions {
            include_system_prompt: true,
            ..Default::default()
        };
        let result = conversation_to_sft_jsonl(&conv, Some(&cfg), &opts).unwrap();
        let val = result.unwrap();

        let messages = val["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn sft_min_messages_filter() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hi")],
            vec![None],
            vec![None],
            vec![],
        );
        let opts = SftExportOptions {
            min_messages: 2,
            ..Default::default()
        };
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sft_strips_multimodal_content() {
        use rig::completion::message::{DocumentSourceKind, Image, ImageMediaType};

        let user_content = OneOrMany::many(vec![
            UserContent::Text(Text {
                text: "Look at this".to_string(),
            }),
            UserContent::Image(Image {
                data: DocumentSourceKind::Base64("fake-base64".to_string()),
                media_type: Some(ImageMediaType::JPEG),
                detail: None,
                additional_params: None,
            }),
        ])
        .unwrap();

        let history = vec![
            Message::User {
                content: user_content,
            },
            assistant_message("I see the image"),
        ];

        let conv = make_conversation_data(
            "conv-1",
            "m",
            history,
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let opts = SftExportOptions::default();
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        let val = result.unwrap();

        let messages = val["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["content"], "Look at this");
    }

    #[test]
    fn sft_includes_tool_calls() {
        use rig::completion::message::{ToolCall, ToolFunction};

        let trace = crate::chatty::views::message_types::SystemTrace {
            items: vec![TraceItem::ToolCall(ToolCallBlock {
                id: "tc_1".to_string(),
                tool_name: "read_file".to_string(),
                display_name: "read_file".to_string(),
                input: "{}".to_string(),
                output: Some("file contents".to_string()),
                output_preview: None,
                state: ToolCallState::Success,
                duration: None,
                text_before: String::new(),
            })],
            total_duration: None,
            active_tool_index: None,
        };

        let history = vec![
            user_message("Read the file"),
            Message::Assistant {
                id: None,
                content: OneOrMany::many(vec![
                    AssistantContent::ToolCall(ToolCall {
                        id: "tc_1".to_string(),
                        call_id: Some("call_abc".to_string()),
                        function: ToolFunction {
                            name: "read_file".to_string(),
                            arguments: serde_json::json!({"path": "/tmp/file.txt"}),
                        },
                        signature: None,
                        additional_params: None,
                    }),
                    AssistantContent::Text(Text {
                        text: "Here is the file".to_string(),
                    }),
                ])
                .unwrap(),
            },
        ];

        let conv = make_conversation_data(
            "conv-1",
            "m",
            history,
            vec![None, Some(serde_json::to_value(&trace).unwrap())],
            vec![None, None],
            vec![],
        );
        let opts = SftExportOptions {
            include_tool_calls: true,
            ..Default::default()
        };
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        let val = result.unwrap();

        let messages = val["messages"].as_array().unwrap();
        // user + assistant (with tool_calls) + tool result = 3
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(messages[1]["content"], "Here is the file");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_abc");
        assert_eq!(messages[2]["content"], "file contents");
    }

    #[test]
    fn sft_empty_conversation_returns_none() {
        let conv = make_conversation_data("conv-1", "m", vec![], vec![], vec![], vec![]);
        let opts = SftExportOptions::default();
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sft_conversation_id_present() {
        let conv = make_conversation_data(
            "my-unique-id",
            "m",
            vec![user_message("Hi"), assistant_message("Hello")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let opts = SftExportOptions::default();
        let result = conversation_to_sft_jsonl(&conv, None, &opts).unwrap();
        let val = result.unwrap();
        assert_eq!(val["_conversation_id"], "my-unique-id");
    }

    // ── DPO tests ─────────────────────────────────────────────────────

    #[test]
    fn dpo_no_regenerations_returns_empty() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hi"), assistant_message("Hello")],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let result = conversation_to_dpo_jsonl(&conv, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn dpo_produces_preference_pair() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hi"), assistant_message("New response")],
            vec![None, None],
            vec![None, None],
            vec![RegenerationRecord {
                message_index: 1,
                original_text: "Old response".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700000010,
            }],
        );
        let result = conversation_to_dpo_jsonl(&conv, None).unwrap();
        assert_eq!(result.len(), 1);

        let pair = &result[0];
        assert_eq!(pair["chosen"], "New response");
        assert_eq!(pair["rejected"], "Old response");
        assert_eq!(pair["_conversation_id"], "conv-1");

        let prompt = pair["prompt"].as_array().unwrap();
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0]["role"], "user");
        assert_eq!(prompt[0]["content"], "Hi");
    }

    #[test]
    fn dpo_with_system_prompt() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![user_message("Hi"), assistant_message("New response")],
            vec![None, None],
            vec![None, None],
            vec![RegenerationRecord {
                message_index: 1,
                original_text: "Old response".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700000010,
            }],
        );
        let cfg = make_model_config("Be helpful.");
        let result = conversation_to_dpo_jsonl(&conv, Some(&cfg)).unwrap();
        assert_eq!(result.len(), 1);

        let prompt = result[0]["prompt"].as_array().unwrap();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[0]["role"], "system");
        assert_eq!(prompt[0]["content"], "Be helpful.");
        assert_eq!(prompt[1]["role"], "user");
    }

    #[test]
    fn dpo_multiple_regenerations() {
        let conv = make_conversation_data(
            "conv-1",
            "m",
            vec![
                user_message("Hi"),
                assistant_message("Response v3"),
                user_message("Follow up"),
                assistant_message("Another response"),
            ],
            vec![None, None, None, None],
            vec![None, None, None, None],
            vec![
                RegenerationRecord {
                    message_index: 1,
                    original_text: "Response v1".to_string(),
                    original_timestamp: 1700000000,
                    regeneration_timestamp: 1700000010,
                },
                RegenerationRecord {
                    message_index: 1,
                    original_text: "Response v2".to_string(),
                    original_timestamp: 1700000010,
                    regeneration_timestamp: 1700000020,
                },
            ],
        );
        let result = conversation_to_dpo_jsonl(&conv, None).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["rejected"], "Response v1");
        assert_eq!(result[0]["chosen"], "Response v3");
        assert_eq!(result[1]["rejected"], "Response v2");
        assert_eq!(result[1]["chosen"], "Response v3");
    }

    #[test]
    fn dpo_conversation_id_present() {
        let conv = make_conversation_data(
            "my-dpo-id",
            "m",
            vec![user_message("Hi"), assistant_message("New")],
            vec![None, None],
            vec![None, None],
            vec![RegenerationRecord {
                message_index: 1,
                original_text: "Old".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700000010,
            }],
        );
        let result = conversation_to_dpo_jsonl(&conv, None).unwrap();
        assert_eq!(result[0]["_conversation_id"], "my-dpo-id");
    }

    #[test]
    fn malformed_message_history_returns_err() {
        let conv = ConversationData {
            id: "id".to_string(),
            title: "Test".to_string(),
            model_id: "m".to_string(),
            message_history: "not json".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "{}".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            message_feedback: "[]".to_string(),
            regeneration_records: "[]".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        let opts = SftExportOptions::default();
        assert!(conversation_to_sft_jsonl(&conv, None, &opts).is_err());
        assert!(conversation_to_dpo_jsonl(&conv, None).is_err());
    }
}
