use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rig::completion::Message;
use rig::completion::message::{AssistantContent, UserContent};

use crate::chatty::exporters::types::*;
use crate::chatty::models::conversation::{MessageFeedback, RegenerationRecord};
use crate::chatty::models::token_usage::ConversationTokenUsage;
use crate::chatty::repositories::ConversationData;
use crate::chatty::views::message_types::{SystemTrace, TraceItem};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::ProviderType;

/// Convert a persisted conversation into ATIF JSON format.
///
/// This is a pure function with no side effects — it takes data and returns
/// a JSON value without touching global state or performing I/O.
///
/// Phases:
/// 1. Deserialize all double-serialized JSON strings in ConversationData
/// 2. Build the agent block from model_id and optional ModelConfig
/// 3. Iterate through message history, building one ATIF step per message
/// 4. Build final_metrics from ConversationTokenUsage totals
/// 5. Build the extra block (feedback + regenerations)
pub fn conversation_to_atif(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
) -> Result<serde_json::Value> {
    // PHASE 1: Deserialize all parallel arrays
    let history: Vec<Message> = serde_json::from_str(&conversation.message_history)
        .context("Failed to parse message_history")?;
    let traces: Vec<Option<serde_json::Value>> = serde_json::from_str(&conversation.system_traces)
        .context("Failed to parse system_traces")?;
    let token_usage: ConversationTokenUsage =
        serde_json::from_str(&conversation.token_usage).unwrap_or_default();
    let attachment_paths: Vec<Vec<String>> =
        serde_json::from_str(&conversation.attachment_paths).unwrap_or_default();
    let timestamps: Vec<Option<i64>> =
        serde_json::from_str(&conversation.message_timestamps).unwrap_or_default();
    let feedback: Vec<Option<MessageFeedback>> =
        serde_json::from_str(&conversation.message_feedback).unwrap_or_default();
    let regeneration_records: Vec<RegenerationRecord> =
        serde_json::from_str(&conversation.regeneration_records).unwrap_or_default();

    // PHASE 2: Build agent block
    let agent = build_agent(&conversation.model_id, model_config);

    // PHASE 3: Build steps (track assistant turn index for token_usage lookup)
    let mut steps = Vec::with_capacity(history.len());
    let mut assistant_turn_idx: usize = 0;

    for (idx, message) in history.iter().enumerate() {
        let timestamp = timestamps.get(idx).copied().flatten();
        let msg_attachments = attachment_paths.get(idx).cloned().unwrap_or_default();
        let trace_json = traces.get(idx).cloned().flatten();

        let step =
            match message {
                Message::User { content } => build_user_step(content, timestamp, &msg_attachments),
                Message::Assistant { content, .. } => {
                    let metrics = token_usage.message_usages.get(assistant_turn_idx).map(|u| {
                        AtifStepMetrics {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                        }
                    });
                    assistant_turn_idx += 1;
                    build_agent_step(content, timestamp, trace_json, metrics)
                }
            };
        steps.push(step);
    }

    // PHASE 4: Build final_metrics
    let final_metrics = AtifFinalMetrics {
        total_input_tokens: token_usage.total_input_tokens,
        total_output_tokens: token_usage.total_output_tokens,
        total_cost_usd: token_usage.total_estimated_cost_usd,
    };

    // PHASE 5: Build extra (feedback + regenerations)
    let extra = build_extra(&feedback, &regeneration_records);

    let export = AtifExport {
        session_id: conversation.id.clone(),
        agent,
        steps,
        final_metrics,
        extra,
    };

    serde_json::to_value(&export).context("Failed to serialize ATIF export")
}

fn build_agent(model_id: &str, model_config: Option<&ModelConfig>) -> AtifAgent {
    match model_config {
        Some(cfg) => AtifAgent {
            model_name: cfg.model_identifier.clone(),
            provider: provider_name(&cfg.provider_type),
        },
        None => AtifAgent {
            model_name: model_id.to_string(),
            provider: "unknown".to_string(),
        },
    }
}

fn provider_name(provider_type: &ProviderType) -> String {
    match provider_type {
        ProviderType::Anthropic => "anthropic",
        ProviderType::OpenAI => "openai",
        ProviderType::Gemini => "gemini",
        ProviderType::Ollama => "ollama",
        ProviderType::Mistral => "mistral",
        ProviderType::AzureOpenAI => "azure_openai",
    }
    .to_string()
}

fn build_user_step(
    content: &rig::OneOrMany<UserContent>,
    timestamp: Option<i64>,
    attachment_paths: &[String],
) -> AtifStep {
    let text: String = content
        .iter()
        .filter_map(|uc| match uc {
            UserContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let attachments = attachment_paths
        .iter()
        .map(|p| AtifAttachment {
            attachment_type: classify_attachment(Path::new(p)),
            path: p.clone(),
        })
        .collect();

    AtifStep {
        source: "user".to_string(),
        content: text,
        timestamp,
        attachments,
        tool_calls: Vec::new(),
        reasoning_content: None,
        metrics: None,
    }
}

fn build_agent_step(
    content: &rig::OneOrMany<AssistantContent>,
    timestamp: Option<i64>,
    trace_json: Option<serde_json::Value>,
    metrics: Option<AtifStepMetrics>,
) -> AtifStep {
    let text: String = content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    // Parse system trace for reasoning content and tool outputs (keyed by ToolCallBlock.id)
    let (reasoning_content, trace_outputs) = parse_trace(trace_json);

    // Build tool calls from rig AssistantContent, enriching with trace outputs.
    // The rig ToolCall.id matches the trace ToolCallBlock.id, so we use it for lookup
    // before mapping to the ATIF id (which prefers call_id when available).
    let tool_calls: Vec<AtifToolCall> = content
        .iter()
        .filter_map(|ac| match ac {
            AssistantContent::ToolCall(tc) => {
                let output = trace_outputs.get(&tc.id).cloned();
                Some(AtifToolCall {
                    id: tc.call_id.clone().unwrap_or_else(|| tc.id.clone()),
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                    output,
                })
            }
            _ => None,
        })
        .collect();

    AtifStep {
        source: "agent".to_string(),
        content: text,
        timestamp,
        attachments: Vec::new(),
        tool_calls,
        reasoning_content,
        metrics,
    }
}

/// Parse a system trace JSON, returning (reasoning_text, tool_output_by_id).
fn parse_trace(trace_json: Option<serde_json::Value>) -> (Option<String>, HashMap<String, String>) {
    let mut outputs = HashMap::new();
    let trace_json = match trace_json {
        Some(v) => v,
        None => return (None, outputs),
    };

    let trace: SystemTrace = match serde_json::from_value(trace_json) {
        Ok(t) => t,
        Err(_) => return (None, outputs),
    };

    let reasoning_parts: Vec<String> = trace
        .items
        .iter()
        .filter_map(|item| match item {
            TraceItem::Thinking(tb) if !tb.content.is_empty() => Some(tb.content.clone()),
            _ => None,
        })
        .collect();

    let reasoning_content = if reasoning_parts.is_empty() {
        None
    } else {
        Some(reasoning_parts.join("\n\n"))
    };

    for item in &trace.items {
        if let TraceItem::ToolCall(tc) = item
            && let Some(output) = &tc.output
        {
            outputs.insert(tc.id.clone(), output.clone());
        }
    }

    (reasoning_content, outputs)
}

fn classify_attachment(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
    {
        Some(ext) if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp") => {
            "image".to_string()
        }
        _ => "document".to_string(),
    }
}

fn build_extra(
    feedback: &[Option<MessageFeedback>],
    regenerations: &[RegenerationRecord],
) -> AtifExtra {
    let feedback_strings: Vec<Option<String>> = feedback
        .iter()
        .map(|f| match f {
            Some(MessageFeedback::ThumbsUp) => Some("thumbs_up".to_string()),
            Some(MessageFeedback::ThumbsDown) => Some("thumbs_down".to_string()),
            None => None,
        })
        .collect();

    let atif_regenerations: Vec<AtifRegeneration> = regenerations
        .iter()
        .map(|r| AtifRegeneration {
            message_index: r.message_index,
            original_text: r.original_text.clone(),
            timestamp: r.regeneration_timestamp,
        })
        .collect();

    AtifExtra {
        feedback: feedback_strings,
        regenerations: atif_regenerations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::models::token_usage::TokenUsage;
    use crate::chatty::views::message_types::{
        ThinkingBlock, ThinkingState, ToolCallBlock, ToolCallState,
    };
    use rig::OneOrMany;
    use rig::completion::message::Text;
    use std::collections::HashMap;

    fn make_conversation_data(
        id: &str,
        model_id: &str,
        history: Vec<Message>,
        traces: Vec<Option<serde_json::Value>>,
        token_usage: ConversationTokenUsage,
        attachment_paths: Vec<Vec<String>>,
        timestamps: Vec<Option<i64>>,
        feedback: Vec<Option<MessageFeedback>>,
        regeneration_records: Vec<RegenerationRecord>,
    ) -> ConversationData {
        ConversationData {
            id: id.to_string(),
            title: "Test".to_string(),
            model_id: model_id.to_string(),
            message_history: serde_json::to_string(&history).unwrap(),
            system_traces: serde_json::to_string(&traces).unwrap(),
            token_usage: serde_json::to_string(&token_usage).unwrap(),
            attachment_paths: serde_json::to_string(&attachment_paths).unwrap(),
            message_timestamps: serde_json::to_string(&timestamps).unwrap(),
            message_feedback: serde_json::to_string(&feedback).unwrap(),
            regeneration_records: serde_json::to_string(&regeneration_records).unwrap(),
            created_at: 1700000000,
            updated_at: 1700000100,
        }
    }

    fn make_model_config(provider_type: ProviderType) -> ModelConfig {
        ModelConfig {
            id: "test-id".to_string(),
            name: "Test Model".to_string(),
            provider_type,
            model_identifier: "claude-sonnet-4-20250514".to_string(),
            temperature: 0.7,
            preamble: String::new(),
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

    // ── classify_attachment tests ──────────────────────────────────────

    #[test]
    fn classify_attachment_image_extensions() {
        for ext in &["jpg", "jpeg", "png", "gif", "webp", "JPG", "PNG"] {
            let p = format!("file.{}", ext);
            assert_eq!(
                classify_attachment(Path::new(&p)),
                "image",
                "Expected image for .{}",
                ext
            );
        }
    }

    #[test]
    fn classify_attachment_pdf_is_document() {
        assert_eq!(classify_attachment(Path::new("report.pdf")), "document");
    }

    #[test]
    fn classify_attachment_unknown_is_document() {
        assert_eq!(classify_attachment(Path::new("data.csv")), "document");
    }

    #[test]
    fn classify_attachment_no_extension_is_document() {
        assert_eq!(classify_attachment(Path::new("Makefile")), "document");
    }

    // ── provider_name tests ───────────────────────────────────────────

    #[test]
    fn provider_name_all_variants() {
        assert_eq!(provider_name(&ProviderType::Anthropic), "anthropic");
        assert_eq!(provider_name(&ProviderType::OpenAI), "openai");
        assert_eq!(provider_name(&ProviderType::Gemini), "gemini");
        assert_eq!(provider_name(&ProviderType::Ollama), "ollama");
        assert_eq!(provider_name(&ProviderType::Mistral), "mistral");
        assert_eq!(provider_name(&ProviderType::AzureOpenAI), "azure_openai");
    }

    // ── build_extra tests ─────────────────────────────────────────────

    #[test]
    fn build_extra_maps_feedback() {
        let feedback = vec![
            None,
            Some(MessageFeedback::ThumbsUp),
            Some(MessageFeedback::ThumbsDown),
            None,
        ];
        let extra = build_extra(&feedback, &[]);
        assert_eq!(
            extra.feedback,
            vec![
                None,
                Some("thumbs_up".to_string()),
                Some("thumbs_down".to_string()),
                None
            ]
        );
        assert!(extra.regenerations.is_empty());
    }

    #[test]
    fn build_extra_regenerations() {
        let regen = vec![RegenerationRecord {
            message_index: 1,
            original_text: "old response".to_string(),
            original_timestamp: 1700000000,
            regeneration_timestamp: 1700000010,
        }];
        let extra = build_extra(&[], &regen);
        assert_eq!(extra.regenerations.len(), 1);
        assert_eq!(extra.regenerations[0].message_index, 1);
        assert_eq!(extra.regenerations[0].original_text, "old response");
        assert_eq!(extra.regenerations[0].timestamp, 1700000010);
    }

    // ── parse_trace tests ─────────────────────────────────────────────

    #[test]
    fn parse_trace_none_returns_empty() {
        let (reasoning, outputs) = parse_trace(None);
        assert!(reasoning.is_none());
        assert!(outputs.is_empty());
    }

    #[test]
    fn parse_trace_extracts_thinking_content() {
        let trace = SystemTrace {
            items: vec![TraceItem::Thinking(ThinkingBlock {
                content: "I should reason carefully".to_string(),
                summary: "Reasoning".to_string(),
                duration: None,
                state: ThinkingState::Completed,
            })],
            total_duration: None,
            active_tool_index: None,
        };
        let json = serde_json::to_value(&trace).unwrap();
        let (reasoning, _) = parse_trace(Some(json));
        assert_eq!(reasoning.as_deref(), Some("I should reason carefully"));
    }

    #[test]
    fn parse_trace_joins_multiple_thinking_blocks() {
        let trace = SystemTrace {
            items: vec![
                TraceItem::Thinking(ThinkingBlock {
                    content: "First thought".to_string(),
                    summary: "".to_string(),
                    duration: None,
                    state: ThinkingState::Completed,
                }),
                TraceItem::Thinking(ThinkingBlock {
                    content: "Second thought".to_string(),
                    summary: "".to_string(),
                    duration: None,
                    state: ThinkingState::Completed,
                }),
            ],
            total_duration: None,
            active_tool_index: None,
        };
        let json = serde_json::to_value(&trace).unwrap();
        let (reasoning, _) = parse_trace(Some(json));
        assert_eq!(
            reasoning.as_deref(),
            Some("First thought\n\nSecond thought")
        );
    }

    #[test]
    fn parse_trace_extracts_tool_output() {
        let trace = SystemTrace {
            items: vec![TraceItem::ToolCall(ToolCallBlock {
                id: "call_abc".to_string(),
                tool_name: "read_file".to_string(),
                display_name: "read_file".to_string(),
                input: "{}".to_string(),
                output: Some("file contents here".to_string()),
                output_preview: None,
                state: ToolCallState::Success,
                duration: None,
                text_before: String::new(),
            })],
            total_duration: None,
            active_tool_index: None,
        };
        let json = serde_json::to_value(&trace).unwrap();
        let (_, outputs) = parse_trace(Some(json));
        // Keyed by ToolCallBlock.id, not tool_name
        assert_eq!(
            outputs.get("call_abc").map(|s| s.as_str()),
            Some("file contents here")
        );
        assert!(outputs.get("read_file").is_none());
    }

    #[test]
    fn parse_trace_invalid_json_returns_empty() {
        let json = serde_json::json!({"not": "a trace"});
        let (reasoning, outputs) = parse_trace(Some(json));
        assert!(reasoning.is_none());
        assert!(outputs.is_empty());
    }

    // ── conversation_to_atif integration tests ────────────────────────

    #[test]
    fn empty_history_produces_empty_steps() {
        let conv = make_conversation_data(
            "test-uuid",
            "model-1",
            vec![],
            vec![],
            ConversationTokenUsage::default(),
            vec![],
            vec![],
            vec![],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["session_id"], "test-uuid");
        assert_eq!(result["steps"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn session_id_from_conversation() {
        let conv = make_conversation_data(
            "my-uuid-123",
            "model-1",
            vec![],
            vec![],
            ConversationTokenUsage::default(),
            vec![],
            vec![],
            vec![],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["session_id"], "my-uuid-123");
    }

    #[test]
    fn agent_from_model_config() {
        let conv = make_conversation_data(
            "id",
            "model-1",
            vec![],
            vec![],
            ConversationTokenUsage::default(),
            vec![],
            vec![],
            vec![],
            vec![],
        );
        let cfg = make_model_config(ProviderType::Anthropic);
        let result = conversation_to_atif(&conv, Some(&cfg)).unwrap();
        assert_eq!(result["agent"]["model_name"], "claude-sonnet-4-20250514");
        assert_eq!(result["agent"]["provider"], "anthropic");
    }

    #[test]
    fn agent_fallback_without_model_config() {
        let conv = make_conversation_data(
            "id",
            "some-model-id",
            vec![],
            vec![],
            ConversationTokenUsage::default(),
            vec![],
            vec![],
            vec![],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["agent"]["model_name"], "some-model-id");
        assert_eq!(result["agent"]["provider"], "unknown");
    }

    #[test]
    fn user_message_maps_to_user_step() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("Hello!")],
            vec![None],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![Some(1700000000)],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["steps"][0]["source"], "user");
        assert_eq!(result["steps"][0]["content"], "Hello!");
        assert_eq!(result["steps"][0]["timestamp"], 1700000000);
    }

    #[test]
    fn assistant_message_maps_to_agent_step() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![assistant_message("Hi there!")],
            vec![None],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![Some(1700000005)],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["steps"][0]["source"], "agent");
        assert_eq!(result["steps"][0]["content"], "Hi there!");
        assert_eq!(result["steps"][0]["timestamp"], 1700000005);
    }

    #[test]
    fn timestamps_preserved_as_null_when_missing() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("test")],
            vec![None],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert!(result["steps"][0]["timestamp"].is_null());
    }

    #[test]
    fn token_usage_per_step() {
        let mut usage = ConversationTokenUsage::default();
        usage.add_usage(TokenUsage::new(100, 200));

        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("Hi"), assistant_message("Hello")],
            vec![None, None],
            usage,
            vec![vec![], vec![]],
            vec![None, None],
            vec![None, None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        // User step has no metrics
        assert!(result["steps"][0].get("metrics").is_none());
        // Agent step has metrics
        assert_eq!(result["steps"][1]["metrics"]["input_tokens"], 100);
        assert_eq!(result["steps"][1]["metrics"]["output_tokens"], 200);
    }

    #[test]
    fn final_metrics_totals() {
        let mut usage = ConversationTokenUsage::default();
        let mut tu = TokenUsage::new(100, 200);
        tu.estimated_cost_usd = Some(0.005);
        usage.add_usage(tu);
        let mut tu2 = TokenUsage::new(150, 300);
        tu2.estimated_cost_usd = Some(0.010);
        usage.add_usage(tu2);

        let conv = make_conversation_data(
            "id",
            "m",
            vec![
                user_message("Q1"),
                assistant_message("A1"),
                user_message("Q2"),
                assistant_message("A2"),
            ],
            vec![None, None, None, None],
            usage,
            vec![vec![], vec![], vec![], vec![]],
            vec![None, None, None, None],
            vec![None, None, None, None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["final_metrics"]["total_input_tokens"], 250);
        assert_eq!(result["final_metrics"]["total_output_tokens"], 500);
        assert_eq!(result["final_metrics"]["total_cost_usd"], 0.015);
    }

    #[test]
    fn attachments_classified_correctly() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("See attached")],
            vec![None],
            ConversationTokenUsage::default(),
            vec![vec![
                "/tmp/photo.jpg".to_string(),
                "/tmp/doc.pdf".to_string(),
                "/tmp/image.png".to_string(),
            ]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        let attachments = result["steps"][0]["attachments"].as_array().unwrap();
        assert_eq!(attachments.len(), 3);
        assert_eq!(attachments[0]["type"], "image");
        assert_eq!(attachments[0]["path"], "/tmp/photo.jpg");
        assert_eq!(attachments[1]["type"], "document");
        assert_eq!(attachments[2]["type"], "image");
    }

    #[test]
    fn tool_calls_extracted_from_assistant_content() {
        use rig::completion::message::{ToolCall, ToolFunction};

        let history = vec![Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "tc_1".to_string(),
                call_id: Some("call_abc".to_string()),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/file.txt"}),
                },
                signature: None,
                additional_params: None,
            })),
        }];

        let conv = make_conversation_data(
            "id",
            "m",
            history,
            vec![None],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        let tool_calls = result["steps"][0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_abc");
        assert_eq!(tool_calls[0]["name"], "read_file");
        assert_eq!(tool_calls[0]["arguments"]["path"], "/tmp/file.txt");
    }

    #[test]
    fn reasoning_extracted_from_trace() {
        let trace = SystemTrace {
            items: vec![TraceItem::Thinking(ThinkingBlock {
                content: "Let me think about this...".to_string(),
                summary: "Thinking".to_string(),
                duration: None,
                state: ThinkingState::Completed,
            })],
            total_duration: None,
            active_tool_index: None,
        };

        let conv = make_conversation_data(
            "id",
            "m",
            vec![assistant_message("Here is my answer")],
            vec![Some(serde_json::to_value(&trace).unwrap())],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(
            result["steps"][0]["reasoning_content"],
            "Let me think about this..."
        );
    }

    #[test]
    fn tool_output_enriched_from_trace() {
        use rig::completion::message::{ToolCall, ToolFunction};

        let trace = SystemTrace {
            items: vec![TraceItem::ToolCall(ToolCallBlock {
                id: "tc_1".to_string(),
                tool_name: "read_file".to_string(),
                display_name: "read_file".to_string(),
                input: r#"{"path":"/tmp/file.txt"}"#.to_string(),
                output: Some("Hello World".to_string()),
                output_preview: None,
                state: ToolCallState::Success,
                duration: None,
                text_before: String::new(),
            })],
            total_duration: None,
            active_tool_index: None,
        };

        let history = vec![Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "tc_1".to_string(),
                call_id: Some("call_abc".to_string()),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/file.txt"}),
                },
                signature: None,
                additional_params: None,
            })),
        }];

        let conv = make_conversation_data(
            "id",
            "m",
            history,
            vec![Some(serde_json::to_value(&trace).unwrap())],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["steps"][0]["tool_calls"][0]["output"], "Hello World");
    }

    #[test]
    fn duplicate_tool_names_matched_by_id() {
        use rig::completion::message::{ToolCall, ToolFunction};

        // Two read_file calls in one agent step — same tool name, different ids
        let trace = SystemTrace {
            items: vec![
                TraceItem::ToolCall(ToolCallBlock {
                    id: "tc_1".to_string(),
                    tool_name: "read_file".to_string(),
                    display_name: "read_file".to_string(),
                    input: r#"{"path":"/tmp/a.txt"}"#.to_string(),
                    output: Some("contents of A".to_string()),
                    output_preview: None,
                    state: ToolCallState::Success,
                    duration: None,
                    text_before: String::new(),
                }),
                TraceItem::ToolCall(ToolCallBlock {
                    id: "tc_2".to_string(),
                    tool_name: "read_file".to_string(),
                    display_name: "read_file".to_string(),
                    input: r#"{"path":"/tmp/b.txt"}"#.to_string(),
                    output: Some("contents of B".to_string()),
                    output_preview: None,
                    state: ToolCallState::Success,
                    duration: None,
                    text_before: String::new(),
                }),
            ],
            total_duration: None,
            active_tool_index: None,
        };

        let history = vec![Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::ToolCall(ToolCall {
                    id: "tc_1".to_string(),
                    call_id: Some("call_001".to_string()),
                    function: ToolFunction {
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "/tmp/a.txt"}),
                    },
                    signature: None,
                    additional_params: None,
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "tc_2".to_string(),
                    call_id: Some("call_002".to_string()),
                    function: ToolFunction {
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "/tmp/b.txt"}),
                    },
                    signature: None,
                    additional_params: None,
                }),
            ])
            .unwrap(),
        }];

        let conv = make_conversation_data(
            "id",
            "m",
            history,
            vec![Some(serde_json::to_value(&trace).unwrap())],
            ConversationTokenUsage::default(),
            vec![vec![]],
            vec![None],
            vec![None],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        let tool_calls = result["steps"][0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 2);
        // Each tool call gets its own distinct output
        assert_eq!(tool_calls[0]["output"], "contents of A");
        assert_eq!(tool_calls[1]["output"], "contents of B");
        // ATIF ids are the call_ids
        assert_eq!(tool_calls[0]["id"], "call_001");
        assert_eq!(tool_calls[1]["id"], "call_002");
    }

    #[test]
    fn feedback_in_extra() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("Hi"), assistant_message("Hello")],
            vec![None, None],
            ConversationTokenUsage::default(),
            vec![vec![], vec![]],
            vec![None, None],
            vec![None, Some(MessageFeedback::ThumbsUp)],
            vec![],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        let feedback = result["extra"]["feedback"].as_array().unwrap();
        assert_eq!(feedback.len(), 2);
        assert!(feedback[0].is_null());
        assert_eq!(feedback[1], "thumbs_up");
    }

    #[test]
    fn regeneration_records_in_extra() {
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("Hi"), assistant_message("New response")],
            vec![None, None],
            ConversationTokenUsage::default(),
            vec![vec![], vec![]],
            vec![None, None],
            vec![None, None],
            vec![RegenerationRecord {
                message_index: 1,
                original_text: "Old response".to_string(),
                original_timestamp: 1700000000,
                regeneration_timestamp: 1700000010,
            }],
        );
        let result = conversation_to_atif(&conv, None).unwrap();
        let regens = result["extra"]["regenerations"].as_array().unwrap();
        assert_eq!(regens.len(), 1);
        assert_eq!(regens[0]["message_index"], 1);
        assert_eq!(regens[0]["original_text"], "Old response");
        assert_eq!(regens[0]["timestamp"], 1700000010);
    }

    // ── Edge cases ────────────────────────────────────────────────────

    #[test]
    fn malformed_token_usage_defaults_to_zero() {
        let conv = ConversationData {
            id: "id".to_string(),
            title: "Test".to_string(),
            model_id: "m".to_string(),
            message_history: "[]".to_string(),
            system_traces: "[]".to_string(),
            token_usage: "invalid json".to_string(),
            attachment_paths: "[]".to_string(),
            message_timestamps: "[]".to_string(),
            message_feedback: "[]".to_string(),
            regeneration_records: "[]".to_string(),
            created_at: 0,
            updated_at: 0,
        };
        let result = conversation_to_atif(&conv, None).unwrap();
        assert_eq!(result["final_metrics"]["total_input_tokens"], 0);
        assert_eq!(result["final_metrics"]["total_output_tokens"], 0);
        assert_eq!(result["final_metrics"]["total_cost_usd"], 0.0);
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
        assert!(conversation_to_atif(&conv, None).is_err());
    }

    #[test]
    fn shorter_parallel_arrays_dont_panic() {
        // History has 2 messages but timestamps/feedback only have 1
        let conv = make_conversation_data(
            "id",
            "m",
            vec![user_message("Hi"), assistant_message("Hello")],
            vec![None], // shorter than history
            ConversationTokenUsage::default(),
            vec![vec![]],           // shorter than history
            vec![Some(1700000000)], // shorter than history
            vec![None],             // shorter than history
            vec![],
        );
        let result = conversation_to_atif(&conv, None);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["steps"].as_array().unwrap().len(), 2);
        // Second step has no timestamp (out of bounds defaults to None)
        assert!(val["steps"][1]["timestamp"].is_null());
    }

    // ── Snapshot test ─────────────────────────────────────────────────

    #[test]
    fn snapshot_full_conversation() {
        use rig::completion::message::{ToolCall, ToolFunction};

        let trace = SystemTrace {
            items: vec![
                TraceItem::Thinking(ThinkingBlock {
                    content: "The user wants to read a file. I should use the read_file tool."
                        .to_string(),
                    summary: "Planning file read".to_string(),
                    duration: None,
                    state: ThinkingState::Completed,
                }),
                TraceItem::ToolCall(ToolCallBlock {
                    id: "tc_1".to_string(),
                    tool_name: "read_file".to_string(),
                    display_name: "Read File".to_string(),
                    input: r#"{"path":"/tmp/hello.txt"}"#.to_string(),
                    output: Some("Hello, World!".to_string()),
                    output_preview: Some("Hello, World!".to_string()),
                    state: ToolCallState::Success,
                    duration: None,
                    text_before: String::new(),
                }),
            ],
            total_duration: None,
            active_tool_index: None,
        };

        let mut usage = ConversationTokenUsage::default();
        let mut tu = TokenUsage::new(150, 350);
        tu.estimated_cost_usd = Some(0.008);
        usage.add_usage(tu);

        let history = vec![
            user_message("Read the file /tmp/hello.txt"),
            Message::Assistant {
                id: None,
                content: OneOrMany::many(vec![
                    AssistantContent::ToolCall(ToolCall {
                        id: "tc_1".to_string(),
                        call_id: Some("call_001".to_string()),
                        function: ToolFunction {
                            name: "read_file".to_string(),
                            arguments: serde_json::json!({"path": "/tmp/hello.txt"}),
                        },
                        signature: None,
                        additional_params: None,
                    }),
                    AssistantContent::Text(Text {
                        text: "The file contains: Hello, World!".to_string(),
                    }),
                ])
                .unwrap(),
            },
        ];

        let conv = make_conversation_data(
            "snap-uuid-001",
            "claude-sonnet",
            history,
            vec![None, Some(serde_json::to_value(&trace).unwrap())],
            usage,
            vec![vec!["/tmp/screenshot.png".to_string()], vec![]],
            vec![Some(1700000000), Some(1700000005)],
            vec![None, Some(MessageFeedback::ThumbsUp)],
            vec![RegenerationRecord {
                message_index: 1,
                original_text: "Previous answer".to_string(),
                original_timestamp: 1700000003,
                regeneration_timestamp: 1700000005,
            }],
        );

        let cfg = make_model_config(ProviderType::Anthropic);
        let result = conversation_to_atif(&conv, Some(&cfg)).unwrap();
        let actual = serde_json::to_string_pretty(&result).unwrap();
        let expected = include_str!("snapshots/full_conversation.json");
        assert_eq!(actual, expected);
    }
}
