//! Tests for headless-mode helpers (kept separate so the production code
//! file is easier to navigate).

use super::answer_file::*;
use super::recovery::*;
use super::tool_format::*;
use super::*;

use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use std::collections::BTreeSet;

use crate::engine::{ToolCallInfo, ToolCallState};

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
fn detects_retryable_json_errors() {
    assert!(is_retryable_stream_error(
        "CompletionError: JsonError: EOF while parsing a string at line 1 column 7563"
    ));
    assert!(is_retryable_stream_error(
        "CompletionError: HttpError: Invalid status code 503 Service Unavailable with message: server overloaded"
    ));
    assert!(!is_retryable_stream_error("network timeout"));
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
