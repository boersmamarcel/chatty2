//! Tests for headless-mode helpers (kept separate so the production code
//! file is easier to navigate).

// `super::*` already covers the helper modules: `headless/mod.rs` glob-imports
// `answer_file`, `recovery` and `tool_format`, and a child module sees its
// parent's bindings. Naming them again here was redundant.
use super::*;

use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use std::collections::BTreeSet;

use crate::engine::{ToolCallInfo, ToolCallState};

#[test]
fn formats_structured_progress_lines() {
    assert_eq!(
        format_progress_line("tool_started", "read_file", None),
        "CHATTY_PROGRESS\ttool_started\tread_file"
    );
    assert_eq!(
        format_progress_line("tool_finished", "read_file", Some("ok")),
        "CHATTY_PROGRESS\ttool_finished\tread_file\tok"
    );
    assert_eq!(
        format_progress_line("tool_finished", "shell_execute", Some("err")),
        "CHATTY_PROGRESS\ttool_finished\tshell_execute\terr"
    );
}

#[test]
fn formats_tool_call_with_pretty_json_and_error_output() {
    let tc = ToolCallInfo {
        id: "call-1".to_string(),
        name: "shell_execute".to_string(),
        input: r#"{"command":"pwd"}"#.to_string(),
        output: Some("No such file or directory".to_string()),
        state: ToolCallState::Error,
        source: ToolSource::Local,
        execution_engine: Some(ExecutionEngine::Shell),
    };

    assert_eq!(
        format_tool_call_lines(&tc),
        vec![
            "  [tool: shell_execute] [shell (local)] ✗ failed".to_string(),
            "    input".to_string(),
            "      {".to_string(),
            r#"        "command": "pwd""#.to_string(),
            "      }".to_string(),
            "    error".to_string(),
            "      No such file or directory".to_string(),
        ]
    );
}

#[test]
fn keeps_plain_text_payload_lines() {
    assert_eq!(
        tool_payload_lines("stdout line 1\nstderr line 2\n"),
        vec!["stdout line 1".to_string(), "stderr line 2".to_string(),]
    );
}

#[test]
fn stream_error_retry_follows_the_shared_policy() {
    use chatty_core::services::{StreamError, StreamErrorKind};

    assert!(is_retryable_stream_error(&StreamError::new(
        StreamErrorKind::MalformedToolCall,
        "CompletionError: JsonError: EOF while parsing a string at line 1 column 7563",
    )));
    assert!(is_retryable_stream_error(&StreamError::new(
        StreamErrorKind::ProviderStatus(503),
        "CompletionError: HttpError: Invalid status code 503 Service Unavailable with message: server overloaded",
    )));
    assert!(!is_retryable_stream_error(&StreamError::new(
        StreamErrorKind::Other,
        "network timeout",
    )));
}

#[test]
fn detects_answer_file_requirement() {
    assert!(prompt_requires_answer_file(
        "write ONLY the final answer to `/app/answer.txt`"
    ));
    assert!(prompt_requires_answer_file(
        "Create ANSWER.TXT once you are done"
    ));
    assert!(!prompt_requires_answer_file(
        "Explain the result in the terminal"
    ));
}

#[test]
fn tool_budget_stop_only_applies_to_answer_file_tasks() {
    assert!(!prompt_requires_answer_file("Explain the result"));
    assert_eq!(MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION, 16);
}

#[test]
fn normalizes_acronym_letter_answer_from_prompt() {
    let prompt = r#"What does "R" stand for in the three core policies?"#;
    assert_eq!(
        normalize_answer_for_prompt("original research", prompt),
        "research"
    );
    assert_eq!(
        normalize_answer_for_prompt("No original research", prompt),
        "research"
    );
}

#[test]
fn does_not_normalize_research_without_letter_prompt() {
    assert_eq!(
        normalize_answer_for_prompt("original research", "Which policy was violated?"),
        "original research"
    );
}

#[test]
fn strips_stray_sentence_period_from_short_answer_file_answers() {
    let prompt = "Write ONLY the final answer to `/app/answer.txt`. The answer should be a single, short string.";
    assert_eq!(
        normalize_answer_for_prompt("Extremely.", prompt),
        "Extremely"
    );
    assert_eq!(normalize_answer_for_prompt("U.S.", prompt), "U.S.");
    assert_eq!(normalize_answer_for_prompt("3.14", prompt), "3.14");
}

#[test]
fn extracts_labeled_answer_candidate() {
    assert_eq!(
        candidate_from_line(
            "Best: NexPay = 0.63",
            "Write answer in scheme:fee format to /app/answer.txt",
            true
        )
        .as_deref(),
        Some("NexPay:0.63")
    );
    assert_eq!(
        candidate_from_line("Final answer: Not Applicable", "write answer.txt", true).as_deref(),
        Some("Not Applicable")
    );
}

#[test]
fn rejects_verbose_or_error_candidates() {
    assert!(candidate_from_line("Error: something failed", "answer.txt", false).is_none());
    assert!(
        candidate_from_line(
            "Best: this is a long explanatory sentence with far too many words to be a scalar",
            "answer.txt",
            true
        )
        .is_none()
    );
    assert!(
            candidate_from_line(
                "Best: Practices for Choosing an ACI",
                "Answer must be just the selected card scheme and the associated cost rounded to 2 decimals in this format: {card_scheme}:{fee}",
                true
            )
            .is_none()
        );
    assert!(
        candidate_from_line(
            "Merchant characteritics include",
            "Answer must be just the fee rounded to 2 decimals.",
            false
        )
        .is_none()
    );
    assert_eq!(
        candidate_from_line(
            "0.0",
            "Answer must be just the fee rounded to 2 decimals.",
            false
        )
        .as_deref(),
        Some("0.0")
    );
}

#[test]
fn answer_fallback_only_uses_candidate_source_tools() {
    assert!(is_candidate_source_tool("execute_code"));
    assert!(is_candidate_source_tool("query_data"));
    assert!(!is_candidate_source_tool("doc_retriever"));
    assert!(!is_candidate_source_tool("read_file"));
}

#[test]
fn inferred_answer_fallback_is_opt_in() {
    // The default must be conservative because unverified candidates can be
    // worse than a missing answer file. Strictly formatted answer-file tasks
    // are safe enough to salvage because candidates must match the format.
    assert!(!prompt_has_strict_answer_format("write answer.txt"));
    assert!(prompt_has_strict_answer_format(
        "Answer must be just the selected card scheme and the associated cost rounded to 2 decimals in this format: {card_scheme}:{fee}"
    ));
    assert!(candidate_matches_prompt_format(
        "NexPay:0.63",
        "format: {card_scheme}:{fee}"
    ));
    assert!(!candidate_matches_prompt_format(
        "Practices for Choosing an ACI",
        "format: {card_scheme}:{fee}"
    ));
}

#[test]
fn extracts_known_paths_for_finalization() {
    let mut paths = BTreeSet::new();
    collect_paths_from_text(
        r#"{"path":"/app/data/payments.csv","note":"see data/merchant_data.json and ./manual.md"}"#,
        &mut paths,
    );
    assert!(paths.contains("/app/data/payments.csv"));
    assert!(paths.contains("data/merchant_data.json"));
}

/// AGE-242 / D3: `stop_stream()` + `send_message()` in the same breath
/// (what `run_headless`'s loop-pivot and compact-file branches used to do
/// directly) is a silent no-op because `is_streaming` is still true —
/// `pending_loop_pivot_prompt` / `pending_compact_file_prompt` now hold the
/// prompt until `StreamCompleted` confirms the cancellation went through.
///
/// Driving `run_headless`'s own event loop end-to-end here would need a
/// mocked LLM stream (the deferred send itself starts a real one against
/// Ollama); this instead pins the exact `is_streaming` gate that mechanism
/// depends on, directly on `ChatEngine`.
mod deferred_send_after_cancel {
    use super::*;
    use crate::engine::ChatEngineConfig;
    use chatty_core::factories::agent_factory::AgentBuildContext;
    use chatty_core::models::Conversation;
    use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
    use chatty_core::settings::models::models_store::{ModelConfig, ModelsModel};
    use chatty_core::settings::models::module_settings::ModuleSettingsModel;
    use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};

    /// A `ChatEngine` wrapping a real (network-free) `Conversation`. Ollama
    /// client construction is purely local, so this is safe in unit tests.
    async fn test_engine() -> ChatEngine {
        let _ = chatty_core::init_repositories();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut engine = ChatEngine::new(
            ChatEngineConfig {
                model_config: ModelConfig::new(
                    "m1".to_string(),
                    "Test Model".to_string(),
                    ProviderType::Ollama,
                    "llama3.2".to_string(),
                ),
                provider_config: ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
                execution_settings: ExecutionSettingsModel::default(),
                module_settings: ModuleSettingsModel::default(),
                models: ModelsModel::default(),
                providers: Vec::new(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: Vec::new(),
                remote_agents: Vec::new(),
                module_agents: Vec::new(),
                is_sub_agent: false,
                services_loaded: true,
                surface: chatty_core::services::StreamSurface::Headless,
            },
            event_tx,
        );

        let model_config = ModelConfig::new(
            "m1".to_string(),
            "Test Model".to_string(),
            ProviderType::Ollama,
            "llama3.2".to_string(),
        );
        let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        engine.session.set_conversation(Some(
            Conversation::new(
                "c1".to_string(),
                "Test".to_string(),
                &model_config,
                &provider_config,
                AgentBuildContext {
                    mcp_tools: None,
                    exec_settings: None,
                    pending_approvals: None,
                    pending_clarifications: None,
                    pending_write_approvals: None,
                    pending_artifacts: None,
                    shell_session: None,
                    user_secrets: Vec::new(),
                    theme_colors: None,
                    memory_service: None,
                    skill_service: None,
                    search_settings: None,
                    embedding_service: None,
                    allow_sub_agent: false,
                    module_agents: Vec::new(),
                    gateway_port: None,
                    remote_agents: Vec::new(),
                    available_model_ids: Vec::new(),
                    conversation_id: None,
                },
            )
            .await
            .expect("conversation should build without network access"),
        ));
        engine.is_ready = true;
        engine
    }

    #[tokio::test]
    async fn send_message_right_after_stop_stream_is_refused_but_succeeds_once_cancelled() {
        let mut engine = test_engine().await;

        engine.send_message("first turn".to_string());
        assert!(engine.is_streaming);
        let messages_after_first_send = engine.messages.len();

        engine.stop_stream();
        // Immediately re-entering here is exactly the old bug: is_streaming
        // is still true, so this must be a no-op.
        engine.send_message("queued pivot prompt".to_string());
        assert_eq!(
            engine.messages.len(),
            messages_after_first_send,
            "send_message must no-op while is_streaming is still true"
        );

        // What headless's StreamCancelled/StreamCompleted handling actually
        // does after a cancellation: clears is_streaming.
        engine.handle_event(AppEvent::StreamCancelled);
        engine.handle_event(AppEvent::StreamCompleted);
        assert!(!engine.is_streaming);

        engine.send_message("queued pivot prompt".to_string());
        assert!(
            engine.messages.len() > messages_after_first_send,
            "send_message must succeed once the cancellation has completed"
        );
    }
}
