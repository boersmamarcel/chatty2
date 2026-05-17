//! Tests for the ATIF exporter.
//!
//! Split out of `mod.rs` so the production code is easier to navigate.

use super::*;

    use super::*;
    use crate::models::message_types::{
        ThinkingBlock, ThinkingState, ToolCallBlock, ToolCallState, ToolSource,
    };
    use crate::models::token_usage::TokenUsage;
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
            working_dir: None,
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
            max_context_window: None,
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
                source: ToolSource::Local,
                execution_engine: None,
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
        use rig::completion::message::{ToolCall, ToolFunction};

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
