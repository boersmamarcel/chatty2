//! Tests for the ATIF exporter.
//!
//! Split out of `mod.rs` so the production code is easier to navigate.

use super::*;
use crate::exporters::{ExportRun, export_run};

/// The one exporter, read back as JSON (AGE-859).
fn conversation_to_atif(
    conversation: &ConversationData,
    model_config: Option<&ModelConfig>,
) -> Result<serde_json::Value> {
    let text = export_run(&ExportRun {
        root: conversation,
        model_config,
    })?;
    Ok(serde_json::from_str(&text)?)
}

use crate::models::message_types::{
    SystemTrace, ThinkingBlock, ThinkingState, ToolCallBlock, ToolCallState, ToolSource, TraceItem,
};
use crate::models::token_usage::{ApiCallUsage, TokenUsage};
use crate::settings::models::models_store::ModelSource;
use crate::settings::models::providers_store::ProviderType;
use rig_core::completion::message::{AssistantContent, Text, UserContent};
use std::collections::HashMap;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
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
        working_dir: None,
        agent_task_snapshot: None,
        mode: None,
        tool_call_count: 0,
        context_tokens: 0,
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
        cost_per_million_cache_read_tokens: None,
        cost_per_million_cache_write_tokens: None,
        supports_images: true,
        supports_pdf: true,
        supports_temperature: true,
        max_context_window: None,
        source: ModelSource::User,
        is_favorite: false,
        is_default: false,
    }
}

fn user_message(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text::new(text.to_string()))],
    }
}

fn assistant_message(text: &str) -> Message {
    Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text::new(text.to_string()))],
    }
}

// ── format_timestamp tests ────────────────────────────────────────

#[test]
fn format_timestamp_produces_iso8601() {
    assert_eq!(format_timestamp(1700000000), "2023-11-14T22:13:20Z");
}

#[test]
fn format_timestamp_epoch_zero() {
    assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
}

// ── image_media_type tests ────────────────────────────────────────

#[test]
fn image_media_type_image_extensions() {
    assert_eq!(image_media_type(Path::new("f.jpg")), Some("image/jpeg"));
    assert_eq!(image_media_type(Path::new("f.jpeg")), Some("image/jpeg"));
    assert_eq!(image_media_type(Path::new("f.png")), Some("image/png"));
    assert_eq!(image_media_type(Path::new("f.gif")), Some("image/gif"));
    assert_eq!(image_media_type(Path::new("f.webp")), Some("image/webp"));
    assert_eq!(image_media_type(Path::new("f.JPG")), Some("image/jpeg"));
    assert_eq!(image_media_type(Path::new("f.PNG")), Some("image/png"));
}

#[test]
fn image_media_type_non_image_returns_none() {
    assert_eq!(image_media_type(Path::new("report.pdf")), None);
    assert_eq!(image_media_type(Path::new("data.csv")), None);
    assert_eq!(image_media_type(Path::new("Makefile")), None);
}

// ── provider_name tests ───────────────────────────────────────────

#[test]
fn provider_name_all_variants() {
    assert_eq!(provider_name(&ProviderType::OpenRouter), "openrouter");
    assert_eq!(provider_name(&ProviderType::Ollama), "ollama");
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
            source: ToolSource::Local,
            execution_engine: None,
        })],
        total_duration: None,
        active_tool_index: None,
    };
    let json = serde_json::to_value(&trace).unwrap();
    let (_, outputs) = parse_trace(Some(json));
    assert_eq!(
        outputs.get("call_abc").map(|s| s.as_str()),
        Some("file contents here")
    );
    assert!(!outputs.contains_key("read_file"));
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
fn schema_version_present() {
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
    assert_eq!(result["schema_version"], "ATIF-v1.6");
}

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
    let cfg = make_model_config(ProviderType::OpenRouter);
    let result = conversation_to_atif(&conv, Some(&cfg)).unwrap();
    assert_eq!(result["agent"]["name"], "chatty");
    assert!(result["agent"]["version"].as_str().is_some());
    assert_eq!(result["agent"]["model_name"], "claude-sonnet-4-20250514");
    assert_eq!(result["agent"]["extra"]["provider"], "openrouter");
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
    assert_eq!(result["agent"]["name"], "chatty");
    assert_eq!(result["agent"]["model_name"], "some-model-id");
    assert_eq!(result["agent"]["extra"]["provider"], "unknown");
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
    assert_eq!(result["steps"][0]["step_id"], 1);
    assert_eq!(result["steps"][0]["source"], "user");
    assert_eq!(result["steps"][0]["message"], "Hello!");
    assert_eq!(result["steps"][0]["timestamp"], "2023-11-14T22:13:20Z");
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
    assert_eq!(result["steps"][0]["step_id"], 1);
    assert_eq!(result["steps"][0]["source"], "agent");
    assert_eq!(result["steps"][0]["message"], "Hi there!");
    assert_eq!(result["steps"][0]["timestamp"], "2023-11-14T22:13:25Z");
}

#[test]
fn timestamps_omitted_when_missing() {
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
    assert!(result["steps"][0].get("timestamp").is_none());
}

#[test]
fn step_ids_sequential_from_one() {
    let conv = make_conversation_data(
        "id",
        "m",
        vec![
            user_message("Hi"),
            assistant_message("Hello"),
            user_message("Bye"),
        ],
        vec![None, None, None],
        ConversationTokenUsage::default(),
        vec![vec![], vec![], vec![]],
        vec![None, None, None],
        vec![None, None, None],
        vec![],
    );
    let result = conversation_to_atif(&conv, None).unwrap();
    assert_eq!(result["steps"][0]["step_id"], 1);
    assert_eq!(result["steps"][1]["step_id"], 2);
    assert_eq!(result["steps"][2]["step_id"], 3);
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
    // Agent step has metrics with spec-compliant names
    assert_eq!(result["steps"][1]["metrics"]["prompt_tokens"], 100);
    assert_eq!(result["steps"][1]["metrics"]["completion_tokens"], 200);
}

/// Tool round-trips persisted with the turn (AGE-247) produce no steps of
/// their own: the agent step derives calls and observations from the trace,
/// and the per-turn token usage still lines up with the text answers.
#[test]
fn persisted_tool_round_trips_do_not_become_steps() {
    let mut usage = ConversationTokenUsage::default();
    usage.add_usage(TokenUsage::new(100, 200));

    let history = vec![
        user_message("Read it"),
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::text("Let me look."),
                AssistantContent::tool_call("call-1", "read_file", serde_json::json!({})),
            ],
        },
        Message::tool_result("call-1", "read_file", "contents"),
        assistant_message("Let me look.\n\nDone"),
    ];
    let conv = make_conversation_data(
        "id",
        "m",
        history,
        vec![None, None, None, None],
        usage,
        vec![vec![], vec![], vec![], vec![]],
        vec![None, None, None, None],
        vec![None, None, None, None],
        vec![],
    );
    let result = conversation_to_atif(&conv, None).unwrap();
    let steps = result["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["source"], "user");
    assert_eq!(steps[1]["source"], "agent");
    assert_eq!(steps[1]["message"], "Let me look.\n\nDone");
    assert_eq!(steps[1]["metrics"]["prompt_tokens"], 100);
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
    assert_eq!(result["final_metrics"]["total_prompt_tokens"], 250);
    assert_eq!(result["final_metrics"]["total_completion_tokens"], 500);
    assert_eq!(result["final_metrics"]["total_cost_usd"], 0.015);
    assert_eq!(result["final_metrics"]["total_steps"], 4);
    assert!(
        result["final_metrics"]["extra"].is_null(),
        "a provider that reported no cache activity writes no cache block"
    );
}

/// `total_prompt_tokens` folds cached tokens in, so without these the export
/// cannot say whether a prompt was served from cache (AGE-278).
#[test]
fn final_metrics_carry_cache_counts_when_the_provider_reported_them() {
    let mut usage = ConversationTokenUsage::default();
    usage.add_usage(TokenUsage::from_calls(vec![ApiCallUsage {
        turn: 1,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_write_tokens: 900,
        output_tokens: 10,
        reasoning_tokens: 0,
        ..Default::default()
    }]));
    usage.add_usage(TokenUsage::from_calls(vec![ApiCallUsage {
        turn: 1,
        input_tokens: 50,
        cache_read_tokens: 1_200,
        cache_write_tokens: 0,
        output_tokens: 20,
        reasoning_tokens: 0,
        ..Default::default()
    }]));

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

    assert_eq!(result["final_metrics"]["extra"]["cache_read_tokens"], 1_200);
    assert_eq!(result["final_metrics"]["extra"]["cache_write_tokens"], 900);
    // 150 uncached + 1200 read + 900 written.
    assert_eq!(result["final_metrics"]["total_prompt_tokens"], 2_250);
}

#[test]
fn image_attachments_in_message_content_parts() {
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
    // message is an array of ContentParts (images present)
    let message = result["steps"][0]["message"].as_array().unwrap();
    // First part is text
    assert_eq!(message[0]["type"], "text");
    assert_eq!(message[0]["text"], "See attached");
    // Second part is image (jpg — pdf is non-image so excluded)
    assert_eq!(message[1]["type"], "image");
    assert_eq!(message[1]["source"]["media_type"], "image/jpeg");
    assert_eq!(message[1]["source"]["path"], "/tmp/photo.jpg");
    // Third part is image (png)
    assert_eq!(message[2]["type"], "image");
    assert_eq!(message[2]["source"]["media_type"], "image/png");
    assert_eq!(message[2]["source"]["path"], "/tmp/image.png");
    // PDF is not in message parts (non-image)
    assert_eq!(message.len(), 3);
}

#[test]
fn no_image_attachments_produces_plain_string_message() {
    let conv = make_conversation_data(
        "id",
        "m",
        vec![user_message("See attached")],
        vec![None],
        ConversationTokenUsage::default(),
        vec![vec!["/tmp/doc.pdf".to_string()]],
        vec![None],
        vec![None],
        vec![],
    );
    let result = conversation_to_atif(&conv, None).unwrap();
    // message is a plain string (no image attachments)
    assert_eq!(result["steps"][0]["message"], "See attached");
}

#[test]
fn tool_calls_with_observation() {
    use rig_core::completion::message::{ProviderCallId, ToolCall, ToolCallId, ToolFunction};

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
            source: ToolSource::Local,
            execution_engine: None,
        })],
        total_duration: None,
        active_tool_index: None,
    };

    let history = vec![Message::Assistant {
        id: None,
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: ToolCallId::new("tc_1").unwrap(),
            provider: ProviderCallId::new("call_abc"),
            function: ToolFunction {
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "/tmp/file.txt"}),
            },
            signature: None,
            additional_params: None,
        })],
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

    // Tool call uses spec field names
    let tool_calls = result["steps"][0]["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["tool_call_id"], "call_abc");
    assert_eq!(tool_calls[0]["function_name"], "read_file");
    assert_eq!(tool_calls[0]["arguments"]["path"], "/tmp/file.txt");
    // No "output" on tool_call
    assert!(tool_calls[0].get("output").is_none());

    // Tool output in observation
    let observation = &result["steps"][0]["observation"];
    let results = observation["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["source_call_id"], "call_abc");
    assert_eq!(results[0]["content"], "Hello World");
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
fn duplicate_tool_names_matched_by_id() {
    use rig_core::completion::message::{ProviderCallId, ToolCall, ToolCallId, ToolFunction};

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
                source: ToolSource::Local,
                execution_engine: None,
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
                source: ToolSource::Local,
                execution_engine: None,
            }),
        ],
        total_duration: None,
        active_tool_index: None,
    };

    let history = vec![Message::Assistant {
        id: None,
        content: vec![
            AssistantContent::ToolCall(ToolCall {
                id: ToolCallId::new("tc_1").unwrap(),
                provider: ProviderCallId::new("call_001"),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/a.txt"}),
                },
                signature: None,
                additional_params: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: ToolCallId::new("tc_2").unwrap(),
                provider: ProviderCallId::new("call_002"),
                function: ToolFunction {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/b.txt"}),
                },
                signature: None,
                additional_params: None,
            }),
        ],
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
    assert_eq!(tool_calls[0]["tool_call_id"], "call_001");
    assert_eq!(tool_calls[1]["tool_call_id"], "call_002");

    let obs = result["steps"][0]["observation"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(obs.len(), 2);
    assert_eq!(obs[0]["content"], "contents of A");
    assert_eq!(obs[1]["content"], "contents of B");
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
        working_dir: None,
        agent_task_snapshot: None,
        mode: None,
        tool_call_count: 0,
        context_tokens: 0,
    };
    let result = conversation_to_atif(&conv, None).unwrap();
    assert_eq!(result["final_metrics"]["total_prompt_tokens"], 0);
    assert_eq!(result["final_metrics"]["total_completion_tokens"], 0);
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
        working_dir: None,
        agent_task_snapshot: None,
        mode: None,
        tool_call_count: 0,
        context_tokens: 0,
    };
    assert!(conversation_to_atif(&conv, None).is_err());
}

#[test]
fn shorter_parallel_arrays_dont_panic() {
    let conv = make_conversation_data(
        "id",
        "m",
        vec![user_message("Hi"), assistant_message("Hello")],
        vec![None],
        ConversationTokenUsage::default(),
        vec![vec![]],
        vec![Some(1700000000)],
        vec![None],
        vec![],
    );
    let result = conversation_to_atif(&conv, None);
    assert!(result.is_ok());
    let val = result.unwrap();
    assert_eq!(val["steps"].as_array().unwrap().len(), 2);
    // Second step has no timestamp (out of bounds)
    assert!(val["steps"][1].get("timestamp").is_none());
}

// ── Snapshot test ─────────────────────────────────────────────────

#[test]
fn snapshot_full_conversation() {
    use rig_core::completion::message::{ProviderCallId, ToolCall, ToolCallId, ToolFunction};

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
                source: ToolSource::Local,
                execution_engine: None,
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
            content: vec![
                AssistantContent::ToolCall(ToolCall {
                    id: ToolCallId::new("tc_1").unwrap(),
                    provider: ProviderCallId::new("call_001"),
                    function: ToolFunction {
                        name: "read_file".to_string(),
                        arguments: serde_json::json!({"path": "/tmp/hello.txt"}),
                    },
                    signature: None,
                    additional_params: None,
                }),
                AssistantContent::Text(Text::new("The file contains: Hello, World!")),
            ],
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

    let cfg = make_model_config(ProviderType::OpenRouter);
    let result = conversation_to_atif(&conv, Some(&cfg)).unwrap();
    let mut expected: serde_json::Value =
        serde_json::from_str(include_str!("../snapshots/full_conversation.json")).unwrap();

    // Normalize version field so the snapshot doesn't break on version bumps
    let mut actual = result.clone();
    actual["agent"]["version"] = serde_json::json!("VERSION");
    expected["agent"]["version"] = serde_json::json!("VERSION");

    // Semantic comparison: key order doesn't matter
    assert_eq!(actual, expected);
}

/// The full-run export (AGE-859): every agent's steps nested under the
/// delegation step that started it, from the turn's agents as the session
/// keeps them on its trace.
mod full_run {
    use super::*;
    use crate::models::token_usage::ModelRef;
    use crate::services::swarm_trace::{AGENTS_TRACE_KEY, SwarmTrace};
    use crate::session::SessionEvent;
    use crate::tools::invoke_agent_tool::{InvokeAgentProgress, STOPPED_BY_USER};
    use chatty_fabric::{CallChain, CapturedConversation, SwarmEvent, SwarmItem};
    use rig_core::completion::message::{
        Reasoning, ToolCall, ToolCallId, ToolFunction, ToolResult, ToolResultContent,
    };
    use serde_json::{Value, json};

    fn model() -> ModelRef {
        ModelRef {
            provider: ProviderType::Ollama,
            model_id: "m".to_string(),
        }
    }

    fn usage(input: u32, output: u32, delegated_to: Option<&str>) -> TokenUsage {
        let mut usage = TokenUsage::new(input, output);
        usage.model = Some(model());
        usage.delegated_to = delegated_to.map(str::to_string);
        usage
    }

    fn call(id: &str, name: &str, arguments: Value) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall {
            id: ToolCallId::new(id).unwrap(),
            provider: None,
            function: ToolFunction {
                name: name.to_string(),
                arguments,
            },
            signature: None,
            additional_params: None,
        })
    }

    fn result(id: &str, name: &str, text: &str) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::new(id).unwrap(),
                name: name.to_string(),
                content: vec![ToolResultContent::text(text)],
                provider: None,
            })],
        }
    }

    /// A worker's captured conversation: it thinks, reads a file, maybe
    /// delegates to `callee`, and answers.
    fn worker(callee: Option<&str>) -> CapturedConversation {
        let mut messages = vec![
            Message::user("write it"),
            Message::Assistant {
                id: None,
                content: vec![
                    AssistantContent::Reasoning(Reasoning::new("thinking about it")),
                    call("w-read", "read_file", json!({ "path": "main.rs" })),
                ],
            },
            result("w-read", "read_file", "fn main() {}"),
        ];
        if let Some(callee) = callee {
            messages.push(Message::Assistant {
                id: None,
                content: vec![call(
                    "w-call",
                    "invoke_agent",
                    json!({ "agent": callee, "prompt": "test it" }),
                )],
            });
            messages.push(result("w-call", "invoke_agent", "tests pass"));
        }
        messages.push(Message::assistant("done"));
        CapturedConversation::Messages {
            messages: serde_json::to_value(messages).unwrap(),
        }
    }

    /// The root's turn: its delegation tool calls, the events its
    /// delegations sent, and the usage the conversation recorded.
    struct Turn {
        trace: SwarmTrace,
        calls: Vec<AssistantContent>,
        outputs: Vec<(String, String, String)>,
        usage: ConversationTokenUsage,
    }

    impl Turn {
        fn new(own: TokenUsage) -> Self {
            let mut usage = ConversationTokenUsage::new();
            usage.add_usage(own);
            Self {
                trace: SwarmTrace::new(),
                calls: Vec::new(),
                outputs: Vec::new(),
                usage,
            }
        }

        fn start_call(&mut self, id: &str, name: &str, arguments: Value) {
            self.trace.apply(&SessionEvent::ToolCallStarted {
                id: id.into(),
                name: name.into(),
            });
            self.trace.apply(&SessionEvent::ToolCallInput {
                id: id.into(),
                arguments: arguments.to_string(),
            });
            self.calls.push(call(id, name, arguments));
        }

        fn end_call(&mut self, id: &str, name: &str, output: &str) {
            self.trace.apply(&SessionEvent::ToolCallResult {
                id: id.into(),
                result: output.into(),
            });
            self.outputs
                .push((id.to_string(), name.to_string(), output.to_string()));
        }

        fn progress(&mut self, progress: InvokeAgentProgress) {
            self.trace.apply(&SessionEvent::Delegation(progress));
        }

        fn started(&mut self, agent: &str, prompt: &str) {
            self.progress(InvokeAgentProgress::Started {
                agent_name: agent.into(),
                prompt: prompt.into(),
                source: ToolSource::Local,
            });
        }

        /// A delegation ending, with what it captured and spent.
        fn finished(
            &mut self,
            agent: &str,
            conversation: Option<CapturedConversation>,
            success: bool,
            result: Option<&str>,
            spent: Option<(u32, u32)>,
        ) {
            if let Some(conversation) = conversation {
                self.progress(InvokeAgentProgress::Conversation(conversation));
            }
            let lines: Vec<TokenUsage> = spent
                .map(|(i, o)| usage(i, o, Some(agent)))
                .into_iter()
                .collect();
            for line in &lines {
                self.usage.add_usage(line.clone());
            }
            self.progress(InvokeAgentProgress::Finished {
                success,
                result: result.map(str::to_string),
                usage: lines,
            });
        }

        fn delegate(&mut self, id: &str, agent: &str, spent: (u32, u32)) {
            let prompt = format!("task for {agent}");
            self.start_call(id, "invoke_agent", json!({ "agent": agent, "prompt": prompt }));
            self.started(agent, &prompt);
            self.finished(agent, Some(worker(None)), true, Some("done"), Some(spent));
            self.end_call(id, "invoke_agent", "done");
        }

        /// The conversation the session would have persisted for the turn.
        fn conversation(self) -> ConversationData {
            let items: Vec<TraceItem> = self
                .outputs
                .iter()
                .map(|(id, name, output)| {
                    TraceItem::ToolCall(ToolCallBlock {
                        id: id.clone(),
                        tool_name: name.clone(),
                        display_name: name.clone(),
                        input: String::new(),
                        output: Some(output.clone()),
                        output_preview: None,
                        state: ToolCallState::Success,
                        duration: None,
                        text_before: String::new(),
                        source: ToolSource::Local,
                        execution_engine: None,
                    })
                })
                .collect();
            let mut trace = serde_json::to_value(SystemTrace {
                items,
                total_duration: None,
                active_tool_index: None,
            })
            .unwrap();
            let mut records = self.trace.records();
            records[0].node.tool_calls.clear();
            trace[AGENTS_TRACE_KEY] = serde_json::to_value(records).unwrap();
            let mut content = self.calls;
            content.push(AssistantContent::text("all done"));
            make_conversation_data(
                "run",
                "m",
                vec![
                    Message::user("build it"),
                    Message::Assistant { id: None, content },
                ],
                vec![None, Some(trace)],
                self.usage,
                vec![vec![], vec![]],
                vec![None, None],
                vec![None, None],
                vec![],
            )
        }
    }

    fn export_of(conversation: &ConversationData) -> Value {
        conversation_to_atif(conversation, None).unwrap()
    }

    fn steps_of<'a>(export: &'a Value, agent: &str) -> Vec<&'a Value> {
        export["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["extra"]["agent"] == agent)
            .collect()
    }

    fn roster_entry<'a>(export: &'a Value, path: &str) -> &'a Value {
        export["extra"]["swarm"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["path"] == path)
            .unwrap_or_else(|| panic!("{path} is not in the roster"))
    }

    /// root → coder → tester: the root's delegation, then coder's steps
    /// under it, then tester's under coder's own `invoke_agent` step.
    fn nested_run() -> Value {
        let mut turn = Turn::new(usage(100, 10, None));
        turn.start_call(
            "call-1",
            "invoke_agent",
            json!({ "agent": "coder", "prompt": "write" }),
        );
        turn.started("coder", "write");
        let chain = CallChain::root("t-1")
            .extend("coder")
            .unwrap()
            .extend("tester")
            .unwrap();
        turn.trace.apply(&SessionEvent::SwarmEvent(SwarmEvent {
            root_task_id: "t-1".into(),
            node: "tester-0".into(),
            chain,
            inner: vec![
                SwarmItem::TurnStarted,
                SwarmItem::Conversation {
                    conversation: worker(None),
                },
                SwarmItem::Usage {
                    usage: json!({ "lines": [{
                        "model": { "provider": "ollama", "model_id": "m" },
                        "inputTokens": 20, "outputTokens": 2,
                    }] }),
                },
                SwarmItem::Ended {
                    state: "completed".into(),
                },
            ],
        }));
        // What coder reports is its whole subtree's: 40/4 own + tester's.
        turn.finished(
            "coder",
            Some(worker(Some("tester"))),
            true,
            Some("done"),
            Some((60, 6)),
        );
        turn.end_call("call-1", "invoke_agent", "done");
        export_of(&turn.conversation())
    }

    #[test]
    fn export_nests_worker_turns_under_delegation() {
        let export = nested_run();
        let root = steps_of(&export, "root");
        assert_eq!(root.len(), 2, "the user's message and the leader's turn");
        let delegation = root[1]["step_id"].as_u64().unwrap();
        assert_eq!(root[1]["tool_calls"][0]["function_name"], "invoke_agent");

        let coder = steps_of(&export, "root/coder");
        assert!(!coder.is_empty(), "coder's turns are in the export");
        for step in &coder {
            assert_eq!(step["extra"]["parent_step"], delegation);
        }
        assert_eq!(coder[0]["step_id"].as_u64(), Some(delegation + 1));

        let coder_call = coder
            .iter()
            .find(|s| s["tool_calls"][0]["function_name"] == "invoke_agent")
            .expect("coder's own delegation")["step_id"]
            .as_u64()
            .unwrap();
        let tester = steps_of(&export, "root/coder/tester-0");
        assert!(!tester.is_empty(), "tester's turns are in the export");
        for step in &tester {
            assert_eq!(step["extra"]["parent_step"], coder_call);
        }
        assert_eq!(tester[0]["step_id"].as_u64(), Some(coder_call + 1));

        // Step ids are the document's order.
        let ids: Vec<u64> = export["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["step_id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, (1..=ids.len() as u64).collect::<Vec<_>>());
        assert_eq!(roster_entry(&export, "root/coder/tester-0")["parent"], 1);
    }

    #[test]
    fn export_includes_worker_reasoning_and_tool_results() {
        let export = nested_run();
        for agent in ["root/coder", "root/coder/tester-0"] {
            let steps = steps_of(&export, agent);
            let read = steps
                .iter()
                .find(|s| s["tool_calls"][0]["function_name"] == "read_file")
                .unwrap_or_else(|| panic!("{agent}'s read_file step"));
            assert_eq!(read["reasoning_content"], "thinking about it");
            assert_eq!(read["tool_calls"][0]["arguments"]["path"], "main.rs");
            assert_eq!(read["observation"]["results"][0]["content"], "fn main() {}");
            assert_eq!(
                read["observation"]["results"][0]["source_call_id"],
                "w-read"
            );
            assert!(steps.iter().any(|s| s["message"] == "done"), "its answer");
            assert!(
                steps.iter().any(|s| s["source"] == "user" && s["message"] == "write it"),
                "the task it was given"
            );
        }
    }

    #[test]
    fn best_of_attempts_and_judge_are_children() {
        let mut turn = Turn::new(usage(100, 10, None));
        turn.start_call("call-b", "best_of", json!({ "task": "solve" }));
        for agent in ["solver-a", "solver-b"] {
            turn.started(agent, "solve");
            turn.finished(agent, Some(worker(None)), true, Some("42"), Some((30, 3)));
        }
        turn.started("judge", "pick one");
        turn.finished("judge", Some(worker(None)), true, Some("2"), Some((10, 1)));
        let output = json!({
            "answer": "42", "chosen": 2, "selected_by": "judge",
            "reason": "the second shows its work",
            "response": "FINAL ANSWER: 42", "cost": "2 attempts + judge",
            "attempts": [
                { "number": 1, "agent": "solver-a", "success": true, "answer": "42", "tokens": 33 },
                { "number": 2, "agent": "solver-b", "success": true, "answer": "42", "tokens": 33 },
            ],
        });
        turn.end_call("call-b", "best_of", &output.to_string());
        let export = export_of(&turn.conversation());

        let best_of = steps_of(&export, "root")[1];
        let id = best_of["step_id"].as_u64().unwrap();
        for agent in ["root/solver-a", "root/solver-b", "root/judge"] {
            let steps = steps_of(&export, agent);
            assert!(!steps.is_empty(), "{agent} is in the export");
            for step in steps {
                assert_eq!(step["extra"]["parent_step"], id, "{agent}");
            }
        }
        let choice = &best_of["extra"]["best_of"];
        assert_eq!(choice["chosen"], 2);
        assert_eq!(choice["selected_by"], "judge");
        assert_eq!(choice["reason"], "the second shows its work");
        assert_eq!(choice["judge"], "root/judge");
        assert!(export["extra"]["incomplete"].is_null(), "{export:#}");
    }

    #[test]
    fn failed_and_stopped_workers_are_exported_with_status() {
        let mut turn = Turn::new(usage(100, 10, None));
        turn.start_call(
            "call-f",
            "invoke_agent",
            json!({ "agent": "coder", "prompt": "write" }),
        );
        turn.started("coder", "write");
        turn.finished("coder", Some(worker(None)), false, Some("⚠️ boom"), Some((40, 4)));
        turn.end_call("call-f", "invoke_agent", "Agent 'coder' reported failure: boom");
        turn.start_call(
            "call-s",
            "invoke_agent",
            json!({ "agent": "reviewer", "prompt": "review" }),
        );
        turn.started("reviewer", "review");
        // A stopped worker hands back no conversation, but what it spent
        // until then still counts.
        turn.finished("reviewer", None, false, Some(STOPPED_BY_USER), Some((5, 0)));
        turn.end_call("call-s", "invoke_agent", STOPPED_BY_USER);
        let export = export_of(&turn.conversation());

        assert_eq!(roster_entry(&export, "root/coder")["status"]["state"], "failed");
        assert_eq!(
            roster_entry(&export, "root/reviewer")["status"]["state"],
            "canceled"
        );
        let coder = steps_of(&export, "root/coder");
        assert!(coder.iter().any(|s| s["message"] == "done"), "its turns");
        let reviewer = steps_of(&export, "root/reviewer");
        assert_eq!(reviewer.len(), 1, "{reviewer:#?}");
        assert!(
            reviewer[0]["message"]
                .as_str()
                .unwrap()
                .contains("was stopped"),
            "{reviewer:#?}"
        );
        assert_eq!(reviewer[0]["metrics"]["prompt_tokens"], 5);
        let incomplete = export["extra"]["incomplete"].to_string();
        assert!(
            incomplete.contains("root/reviewer: no conversation was captured (was stopped)"),
            "{incomplete}"
        );
        // Usage still reconciles: only the missing conversation is noted.
        assert!(!incomplete.contains("step usage"), "{incomplete}");
    }

    #[test]
    fn usage_sums_reconcile_or_export_marked_incomplete() {
        // Complete: step usage is the conversation's total, root, workers
        // and the worker's own worker included.
        let export = nested_run();
        assert!(export["extra"]["incomplete"].is_null(), "{export:#}");
        let sum = |key: &str| -> u64 {
            export["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|s| s["metrics"][key].as_u64())
                .sum()
        };
        assert_eq!(sum("prompt_tokens"), 160);
        assert_eq!(sum("completion_tokens"), 16);
        assert_eq!(export["final_metrics"]["total_prompt_tokens"], 160);
        assert_eq!(export["final_metrics"]["total_completion_tokens"], 16);

        // A conversation that recorded a worker's usage but not its agents
        // (from before AGE-859): marked incomplete, with both reasons.
        let mut turn = Turn::new(usage(100, 10, None));
        turn.delegate("call-1", "coder", (40, 4));
        let mut conversation = turn.conversation();
        let mut traces: Vec<Option<Value>> =
            serde_json::from_str(&conversation.system_traces).unwrap();
        if let Some(Some(trace)) = traces.get_mut(1) {
            trace.as_object_mut().unwrap().remove(AGENTS_TRACE_KEY);
        }
        conversation.system_traces = serde_json::to_string(&traces).unwrap();
        let old = export_of(&conversation);
        let incomplete = old["extra"]["incomplete"].to_string();
        assert!(
            incomplete.contains("step usage sums to 100 prompt and 10 completion tokens; final_metrics has 140 and 14"),
            "{incomplete}"
        );
        assert!(
            incomplete.contains("step 2 delegated (invoke_agent) but has no child trajectory"),
            "{incomplete}"
        );
    }
}
