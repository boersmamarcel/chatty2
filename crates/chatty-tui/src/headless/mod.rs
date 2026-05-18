//! Headless / pipe modes for `chatty-tui`.
//!
//! The TUI binary supports running without the Ratatui UI for scripting and
//! testing scenarios:
//!
//! - **`--headless`** — drive a single conversation programmatically with
//!   flags (model, prompt, tools), print the final assistant message to
//!   stdout, exit.
//! - **`--pipe`** — read a JSON-encoded request from stdin, run the
//!   conversation, write a JSON-encoded response to stdout.
//!
//! # What lives here
//!
//! - `run_headless`, `run_pipe` — the two entry functions called from `main`.
//! - Helpers that wire `ChatEngine` events into stdout-only output (tool
//!   call summaries, token usage, errors).
//!
//! # What does NOT live here
//!
//! - The interactive Ratatui UI — `ui/`.
//! - The shared chat engine — `engine.rs`.
//! - LLM streaming primitives — `chatty_core::services` and `factories`.

use anyhow::Result;
use chatty_core::services::AgentLoopGuard;
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, ToolCallState};
use crate::events::AppEvent;

const MAX_STREAM_ERROR_RECOVERY_ATTEMPTS: usize = 5;
const MAX_MALFORMED_JSON_RECOVERY_ATTEMPTS: usize = 2;
const MAX_FINALIZATION_ATTEMPTS: usize = 4;
const MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 16;
const MAX_FAILED_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 3;
const FINALIZATION_MAX_AGENT_TURNS: u32 = 12;
const FINALIZATION_ORIGINAL_PROMPT_CHARS: usize = 6_000;
const FINALIZATION_EVIDENCE_CHARS: usize = 16_000;
const FINALIZATION_TOOL_OUTPUT_CHARS: usize = 4_000;
const TEXT_HARD_STOP_BYTES: usize = 20_000;
const TEXT_OVERFLOW_RECOVERY_PROMPT: &str = "Stop reasoning — make ONE tool call now. If you already have the answer, call final_answer immediately. Do not write any analysis text before the tool call.";
const STREAM_ERROR_RECOVERY_PROMPT: &str = "A provider stream error interrupted the prior response, but the conversation history and tool results above are still valid. Do not say you lack context. Continue the same benchmark task from the visible evidence. If a complete file extraction or final answer is visible, call final_answer with output_path=/app/answer.txt now. Otherwise use at most one compact tool call and keep output short.";

/// Run in headless mode: send a message, collect the response, print to stdout.
pub async fn run_headless(
    mut engine: ChatEngine,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    message: String,
) -> Result<()> {
    let answer_file_required = prompt_requires_answer_file(&message);

    // Send message
    engine.send_message(message.clone());

    // Collect response
    let mut response = String::new();
    let mut recovery_attempts = 0usize;
    let mut finalization_attempts = 0usize;
    let mut tool_results_since_finalization = 0usize;
    let mut failed_tool_results_since_finalization = 0usize;
    let mut tool_budget_stop_requested = false;
    let mut failure_budget_stop_requested = false;
    let mut compact_file_finalization_sent = false;
    let mut last_compact_file_prompt: Option<String> = None;
    let mut finalization_pending_after_cancel = false;
    let mut recovery_pending_after_error = false;
    let mut pending_recovery_attempt_limit = MAX_STREAM_ERROR_RECOVERY_ATTEMPTS;
    let mut infer_missing_answer = should_infer_missing_answer(&message);
    // Shared loop guard handles: repeated-tool-call detection, late-game deadline,
    // and per-turn verbosity tracking.
    let max_agent_turns = engine.execution_settings.max_agent_turns as usize;
    let mut loop_guard = AgentLoopGuard::new(max_agent_turns, answer_file_required);
    // Hard-stop flag set when loop_guard or the backstop threshold is exceeded.
    let mut text_overflow_stop_requested = false;
    let mut text_hard_stop_requested = false;
    let mut text_bytes_this_turn = 0usize;

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::TextChunk(text) => {
                engine.handle_event(AppEvent::TextChunk(text.clone()));
                // Stream text chunks to stderr so parent process can show live progress.
                eprint!("{}", text);
                response.push_str(&text);
                text_bytes_this_turn += text.len();
                if answer_file_required
                    && !text_hard_stop_requested
                    && text_bytes_this_turn > TEXT_HARD_STOP_BYTES
                {
                    text_hard_stop_requested = true;
                    text_overflow_stop_requested = true;
                    eprintln!(
                        "\nText-only response exceeded hard limit; cancelling stream for compact finalization."
                    );
                    engine.stop_stream();
                }
                if !text_overflow_stop_requested {
                    // Soft stop via loop guard (4 KB).
                    // NOTE: We do NOT call engine.stop_stream() here — interrupting mid-stream
                    // causes JSON parse errors ("EOF while parsing a string at col N").
                    // Instead, we set the flag and inject a recovery prompt after the response
                    // completes naturally in the StreamCompleted handler.
                    if loop_guard.on_text_chunk(text.len()) {
                        text_overflow_stop_requested = true;
                        eprintln!(
                            "\nText-only response exceeded verbosity limit; will inject brevity prompt after response completes."
                        );
                    }
                }
            }
            AppEvent::ToolCallStarted { ref name, .. } => {
                text_bytes_this_turn = 0;
                let name_str = name.clone();
                engine.handle_event(event);
                if let Some(tc) = engine
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|m| m.tool_calls())
                    .find(|tc| tc.name == name_str)
                {
                    eprintln!("\n{}", format_tool_call_header(tc));
                } else {
                    eprintln!("\n  \u{27f3} {}", name_str);
                }
            }
            AppEvent::ToolCallResult { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                let mut called_final_answer = false;
                let mut pivot_msg: Option<String> = None;
                let mut tool_failed = false;
                let mut compact_file_extracted = false;
                if let Some(tc) = engine
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|m| m.tool_calls())
                    .find(|tc| tc.id == id_str)
                {
                    eprintln!();
                    for line in format_tool_call_lines(tc) {
                        eprintln!("{line}");
                    }
                    if tc.name == "final_answer" && answer_file_exists(&engine) {
                        if let Err(error) =
                            normalize_existing_answer_file_for_prompt(&engine, &message)
                        {
                            eprintln!("Answer file post-normalization skipped: {error}");
                        }
                        called_final_answer = true;
                    }
                    tool_failed = tool_result_looks_failed(tc);
                    compact_file_extracted =
                        compact_file_extraction_tool_result(answer_file_required, tc);
                    // Check for repeated identical tool call (loop detection).
                    pivot_msg = loop_guard.on_tool_completed(&tc.name, &tc.input);
                }
                if called_final_answer {
                    eprintln!("final_answer completed and answer file exists; stopping stream.");
                    engine.stop_stream();
                } else if answer_file_required && answer_file_exists(&engine) {
                    // Answer file was written by a non-final_answer tool (e.g. echo via shell).
                    // Stop the stream so the model doesn't loop writing the same answer repeatedly.
                    eprintln!("Answer file exists after tool call; stopping stream early.");
                    engine.stop_stream();
                } else if !compact_file_finalization_sent
                    && compact_file_extracted
                    && !answer_file_exists(&engine)
                {
                    compact_file_finalization_sent = true;
                    infer_missing_answer = true;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    let compact_prompt = build_compact_file_answer_prompt(&engine, &message);
                    last_compact_file_prompt = Some(compact_prompt.clone());
                    eprintln!(
                        "Complete compact file extraction captured; requesting answer from evidence."
                    );
                    engine.stop_stream();
                    send_compact_file_answer_prompt(&mut engine, compact_prompt);
                    continue;
                } else if let Some(pivot) = pivot_msg {
                    eprintln!(
                        "Loop detected (pivot {}/{}): injecting strategy pivot prompt.",
                        loop_guard.loop_pivot_count(),
                        3
                    );
                    engine.stop_stream();
                    engine.send_message(pivot);
                    tool_results_since_finalization = 0;
                    continue;
                }
                tool_results_since_finalization += 1;
                if tool_failed {
                    failed_tool_results_since_finalization += 1;
                }
                if should_stop_for_answer_file_tool_budget(
                    answer_file_required,
                    tool_results_since_finalization,
                    finalization_attempts,
                    tool_budget_stop_requested,
                    &engine,
                ) {
                    tool_budget_stop_requested = true;
                    eprintln!(
                        "Answer file was not created after many tool calls; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                } else if should_stop_for_failed_tool_budget(
                    answer_file_required,
                    failed_tool_results_since_finalization,
                    finalization_attempts,
                    failure_budget_stop_requested,
                    &engine,
                ) {
                    failure_budget_stop_requested = true;
                    eprintln!(
                        "Several tool calls failed without creating an answer file; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                }
            }
            AppEvent::ToolCallError { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                if let Some(tc) = engine
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|m| m.tool_calls())
                    .find(|tc| tc.id == id_str)
                {
                    eprintln!();
                    for line in format_tool_call_lines(tc) {
                        eprintln!("{line}");
                    }
                }
                tool_results_since_finalization += 1;
                failed_tool_results_since_finalization += 1;
                if should_stop_for_answer_file_tool_budget(
                    answer_file_required,
                    tool_results_since_finalization,
                    finalization_attempts,
                    tool_budget_stop_requested,
                    &engine,
                ) {
                    tool_budget_stop_requested = true;
                    eprintln!(
                        "Answer file was not created after many tool calls; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                } else if should_stop_for_failed_tool_budget(
                    answer_file_required,
                    failed_tool_results_since_finalization,
                    finalization_attempts,
                    failure_budget_stop_requested,
                    &engine,
                ) {
                    failure_budget_stop_requested = true;
                    eprintln!(
                        "Several tool calls failed without creating an answer file; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                }
            }
            AppEvent::StreamCompleted => {
                engine.handle_event(AppEvent::StreamCompleted);
                // Update loop guard: resets per-turn counters and checks for late-game deadline.
                let turns_used = engine
                    .messages
                    .iter()
                    .filter(|m| matches!(m.role, crate::engine::MessageRole::Assistant))
                    .count();
                loop_guard.on_turn_complete(turns_used, answer_file_exists(&engine));
                let was_text_overflow = text_overflow_stop_requested;
                text_overflow_stop_requested = false;
                text_hard_stop_requested = false;
                text_bytes_this_turn = 0;
                if recovery_pending_after_error {
                    recovery_pending_after_error = false;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    let delay_secs = 10u64 * recovery_attempts as u64;
                    eprintln!(
                        "Retrying after stream error ({}/{}) in {}s with a compact continuation prompt.",
                        recovery_attempts, pending_recovery_attempt_limit, delay_secs
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    if let Some(compact_prompt) = last_compact_file_prompt.as_deref() {
                        engine.send_message(build_compact_file_recovery_prompt(compact_prompt));
                    } else {
                        engine.send_message(STREAM_ERROR_RECOVERY_PROMPT.to_string());
                    }
                    continue;
                }
                if was_text_overflow {
                    // Model generated too much text without calling a tool (response completed naturally).
                    // Inject a focused action prompt to redirect toward a tool call.
                    if recovery_attempts < MAX_STREAM_ERROR_RECOVERY_ATTEMPTS {
                        recovery_attempts += 1;
                        tool_results_since_finalization = 0;
                        failed_tool_results_since_finalization = 0;
                        tool_budget_stop_requested = false;
                        failure_budget_stop_requested = false;
                        eprintln!(
                            "Text overflow (no tool call after 4KB): injecting action prompt ({}/{}).",
                            recovery_attempts, MAX_STREAM_ERROR_RECOVERY_ATTEMPTS
                        );
                        engine.send_message(TEXT_OVERFLOW_RECOVERY_PROMPT.to_string());
                        continue;
                    }
                    // Exhausted recovery attempts — fall through to finalization.
                }
                // Late-game deadline: inject once when turns are almost exhausted.
                if let Some(deadline) = loop_guard.take_deadline_message() {
                    eprintln!(
                        "Late-game deadline prompt injected ({} turns used of {}).",
                        turns_used, max_agent_turns
                    );
                    engine.send_message(deadline);
                    continue;
                }
                if finalization_pending_after_cancel {
                    finalization_pending_after_cancel = false;
                    finalization_attempts += 1;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    let delay_secs = 15u64 * finalization_attempts as u64;
                    eprintln!(
                        "Answer file was not created after stopping exploration; requesting finalization in {}s.",
                        delay_secs
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    send_answer_file_finalization_prompt(&mut engine, &message);
                    continue;
                }
                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_attempts += 1;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    eprintln!(
                        "Answer file was not created; requesting a compact finalization pass."
                    );
                    send_answer_file_finalization_prompt(&mut engine, &message);
                    continue;
                }
                break;
            }
            AppEvent::StreamError(error) => {
                engine.handle_event(AppEvent::StreamError(error.clone()));
                eprintln!("Error: {}", error);

                if answer_file_exists(&engine) {
                    eprintln!(
                        "Answer file already exists; keeping the run for verifier evaluation."
                    );
                    break;
                }

                let max_recovery_attempts = recovery_attempt_limit_for_error(Some(&error));
                if is_retryable_stream_error(&error) && recovery_attempts < max_recovery_attempts {
                    recovery_attempts += 1;
                    pending_recovery_attempt_limit = max_recovery_attempts;
                    recovery_pending_after_error = true;
                    continue;
                }

                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_pending_after_cancel = true;
                    continue;
                }

                break;
            }
            AppEvent::StreamCancelled => {
                engine.handle_event(AppEvent::StreamCancelled);
                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_pending_after_cancel = true;
                    continue;
                }
                break;
            }
            // Handle other events silently
            _ => {
                engine.handle_event(event);
            }
        }
    }

    if answer_file_required
        && infer_missing_answer
        && !answer_file_exists(&engine)
        && let Some(candidate) = infer_answer_candidate(&engine, &response, &message)
    {
        match write_inferred_answer_file(&engine, &candidate) {
            Ok(path) => eprintln!(
                "Answer file was missing; wrote inferred compact answer candidate '{}' to {}.",
                candidate,
                path.display()
            ),
            Err(error) => eprintln!(
                "Answer file was missing and inferred candidate '{}' could not be written: {}",
                candidate, error
            ),
        }
    }

    // Print response to stdout
    println!("{}", response);

    Ok(())
}

fn should_infer_missing_answer(original_prompt: &str) -> bool {
    match std::env::var("CHATTY_INFER_MISSING_ANSWER")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("0" | "false" | "no") => false,
        Some("1" | "true" | "yes") => true,
        _ => prompt_has_strict_answer_format(original_prompt),
    }
}

fn prompt_has_strict_answer_format(original_prompt: &str) -> bool {
    let prompt = original_prompt.to_ascii_lowercase();
    prompt.contains("answer must be")
        && prompt.contains("format")
        && (prompt.contains(":{") || prompt.contains("}:") || prompt.contains(":"))
}

/// Run in pipe mode: read stdin, send as message, print response to stdout.
pub async fn run_pipe(
    engine: ChatEngine,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        eprintln!("No input provided on stdin");
        std::process::exit(1);
    }

    run_headless(engine, event_rx, input).await
}

mod answer_file;
mod recovery;
mod tool_format;

use answer_file::*;
use recovery::*;
use tool_format::*;

#[cfg(test)]
mod tests;
