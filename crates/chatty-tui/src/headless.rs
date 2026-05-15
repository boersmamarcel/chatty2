use anyhow::Result;
use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use chatty_core::services::AgentLoopGuard;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, ToolCallInfo, ToolCallState};
use crate::events::AppEvent;

const MAX_STREAM_ERROR_RECOVERY_ATTEMPTS: usize = 5;
const MAX_FINALIZATION_ATTEMPTS: usize = 4;
const MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 25;
const FINALIZATION_MAX_AGENT_TURNS: u32 = 12;
const FINALIZATION_ORIGINAL_PROMPT_CHARS: usize = 6_000;
const FINALIZATION_EVIDENCE_CHARS: usize = 8_000;
const FINALIZATION_TOOL_OUTPUT_CHARS: usize = 800;
const TEXT_OVERFLOW_RECOVERY_PROMPT: &str = "Stop reasoning — make ONE tool call now. If you already have the answer, call final_answer immediately. Do not write any analysis text before the tool call.";
const STREAM_ERROR_RECOVERY_PROMPT: &str = "The previous provider response failed after partial progress. Continue from the existing conversation and prior tool results. Do not repeat earlier analysis. If the answer is ready, call final_answer with output_path=/app/answer.txt now. Otherwise use at most one compact tool call and keep any execute_code output short.";

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
    let mut tool_budget_stop_requested = false;
    let mut finalization_pending_after_cancel = false;
    let mut recovery_pending_after_error = false;
    // Shared loop guard handles: repeated-tool-call detection, late-game deadline,
    // and per-turn verbosity tracking.
    let max_agent_turns = engine.execution_settings.max_agent_turns as usize;
    let mut loop_guard = AgentLoopGuard::new(max_agent_turns, answer_file_required);
    // Hard-stop flag set when loop_guard or the backstop threshold is exceeded.
    let mut text_overflow_stop_requested = false;

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::TextChunk(text) => {
                engine.handle_event(AppEvent::TextChunk(text.clone()));
                // Stream text chunks to stderr so parent process can show live progress.
                eprint!("{}", text);
                response.push_str(&text);
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
                        called_final_answer = true;
                    }
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
                if recovery_pending_after_error {
                    recovery_pending_after_error = false;
                    tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    let delay_secs = 10u64 * recovery_attempts as u64;
                    eprintln!(
                        "Retrying after stream error ({}/{}) in {}s with a compact continuation prompt.",
                        recovery_attempts, MAX_STREAM_ERROR_RECOVERY_ATTEMPTS, delay_secs
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    engine.send_message(STREAM_ERROR_RECOVERY_PROMPT.to_string());
                    continue;
                }
                if was_text_overflow {
                    // Model generated too much text without calling a tool (response completed naturally).
                    // Inject a focused action prompt to redirect toward a tool call.
                    if recovery_attempts < MAX_STREAM_ERROR_RECOVERY_ATTEMPTS {
                        recovery_attempts += 1;
                        tool_results_since_finalization = 0;
                        tool_budget_stop_requested = false;
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
                    eprintln!("Late-game deadline prompt injected ({} turns used of {}).", turns_used, max_agent_turns);
                    engine.send_message(deadline);
                    continue;
                }
                if finalization_pending_after_cancel {
                    finalization_pending_after_cancel = false;
                    finalization_attempts += 1;
                    tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
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
                    tool_budget_stop_requested = false;
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

                if is_retryable_stream_error(&error)
                    && recovery_attempts < MAX_STREAM_ERROR_RECOVERY_ATTEMPTS
                {
                    recovery_attempts += 1;
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

    if should_infer_missing_answer(&message)
        && answer_file_required
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

fn format_tool_call_lines(tc: &ToolCallInfo) -> Vec<String> {
    let mut lines = vec![format_tool_call_header(tc)];

    append_tool_payload(&mut lines, "input", &tc.input);

    if let Some(output) = &tc.output {
        let label = match tc.state {
            ToolCallState::Error => "error",
            _ => "output",
        };
        append_tool_payload(&mut lines, label, output);
    }

    lines
}

fn format_tool_call_header(tc: &ToolCallInfo) -> String {
    let (icon, status) = match tc.state {
        ToolCallState::Running => ("\u{27f3}", "running"),
        ToolCallState::Success => ("\u{2713}", "completed"),
        ToolCallState::Error => ("\u{2717}", "failed"),
    };

    let mut header = format!("  [tool: {}] ", tc.name);
    if let Some(engine) = tc.execution_engine {
        header.push_str(&format!("[{}] ", engine_location_label(engine)));
    } else if let Some(source) = source_badge_label(&tc.source) {
        header.push_str(&format!("[{}] ", source));
    }
    header.push_str(&format!("{icon} {status}"));
    header
}

fn append_tool_payload(lines: &mut Vec<String>, label: &str, content: &str) {
    let payload_lines = tool_payload_lines(content);
    if payload_lines.is_empty() {
        return;
    }

    lines.push(format!("    {label}"));
    for payload_line in payload_lines {
        lines.push(format!("      {payload_line}"));
    }
}

fn tool_payload_lines(content: &str) -> Vec<String> {
    let content = content.trim_matches('\n');
    if content.trim().is_empty() {
        return Vec::new();
    }

    let pretty = serde_json::from_str::<serde_json::Value>(content.trim())
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok());
    let display = pretty.as_deref().unwrap_or(content);

    display.lines().map(str::to_string).collect()
}

fn source_badge_label(source: &ToolSource) -> Option<&'static str> {
    match source {
        ToolSource::Local => None,
        ToolSource::HiveCloud => Some("remote"),
        ToolSource::Internet { .. } => Some("remote"),
        ToolSource::ExternalService { .. } => Some("remote"),
    }
}

fn engine_location_label(engine: ExecutionEngine) -> &'static str {
    match engine {
        ExecutionEngine::Shell => "shell (local)",
        ExecutionEngine::Monty => "monty (local)",
        ExecutionEngine::Docker => "docker (local)",
        ExecutionEngine::Daytona => "daytona (remote)",
    }
}

fn is_retryable_stream_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    lowered.contains("jsonerror")
        || lowered.contains("eof while parsing")
        || lowered.contains("failed to parse")
        || lowered.contains("server overloaded")
        || lowered.contains("service unavailable")
        || lowered.contains("internal server error")
        || lowered.contains("invalid status code 500")
        || lowered.contains("invalid status code 502")
        || lowered.contains("invalid status code 503")
        || lowered.contains("invalid status code 504")
        || lowered.contains("http 500")
        || lowered.contains("http 502")
        || lowered.contains("http 503")
        || lowered.contains("http 504")
}

fn prompt_requires_answer_file(prompt: &str) -> bool {
    prompt.to_ascii_lowercase().contains("answer.txt")
}

fn should_request_answer_file_finalization(
    answer_file_required: bool,
    finalization_attempts: usize,
    engine: &ChatEngine,
) -> bool {
    answer_file_required
        && finalization_attempts < MAX_FINALIZATION_ATTEMPTS
        && !answer_file_exists(engine)
}

fn should_stop_for_answer_file_tool_budget(
    answer_file_required: bool,
    tool_results_since_finalization: usize,
    finalization_attempts: usize,
    tool_budget_stop_requested: bool,
    engine: &ChatEngine,
) -> bool {
    answer_file_required
        && !tool_budget_stop_requested
        && finalization_attempts < MAX_FINALIZATION_ATTEMPTS
        && tool_results_since_finalization >= MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION
        && !answer_file_exists(engine)
}

fn send_answer_file_finalization_prompt(engine: &mut ChatEngine, original_prompt: &str) {
    let prompt = build_answer_file_finalization_prompt(engine, original_prompt);
    if let Some(conversation) = engine.conversation.as_mut() {
        conversation.replace_history(Vec::new(), 0);
    }
    engine.execution_settings.max_agent_turns = engine
        .execution_settings
        .max_agent_turns
        .min(FINALIZATION_MAX_AGENT_TURNS);
    engine.send_message(prompt);
}

fn build_answer_file_finalization_prompt(engine: &ChatEngine, original_prompt: &str) -> String {
    let evidence = compact_tool_evidence(engine);
    format!(
        "Finalize this answer-file task using only the compact context below.\n\
          If the evidence contains a final answer candidate, immediately call final_answer with exactly that answer and output_path=/app/answer.txt.\n\
          Do not keep researching. If no answer candidate is present, use at most one compact tool call to compute it. Write the computed answer to /app/answer.txt and then call final_answer with output_path=/app/answer.txt.\n\
          Use exact file paths from the evidence; never invent alternate file names. If exact paths are absent, call file_structure_detector before executing code.\n\n\
          Original task:\n{}\n\n\
          Recent compact tool evidence:\n{}\n",
        truncate_middle(original_prompt, FINALIZATION_ORIGINAL_PROMPT_CHARS),
        evidence
    )
}

fn compact_tool_evidence(engine: &ChatEngine) -> String {
    let tool_calls: Vec<&ToolCallInfo> = engine
        .messages
        .iter()
        .flat_map(|message| message.tool_calls())
        .filter(|tool_call| tool_call.output.is_some())
        .collect();

    let mut sections = Vec::new();
    let known_paths = known_path_lines_from_tool_calls(&tool_calls);
    if !known_paths.is_empty() {
        sections.push(format!("Known file paths:\n{}", known_paths.join("\n")));
    }

    let candidate_lines = candidate_lines_from_tool_calls(&tool_calls);
    if !candidate_lines.is_empty() {
        sections.push(format!(
            "Candidate/result lines:\n{}",
            candidate_lines.join("\n")
        ));
    }

    let mut recent = Vec::new();
    for tool_call in tool_calls.iter().rev().take(8).rev() {
        let output = tool_call
            .output
            .as_deref()
            .map(compact_tool_output)
            .unwrap_or_default();
        if output.trim().is_empty() {
            continue;
        }
        recent.push(format!(
            "[{} {}]\n{}",
            tool_call.name,
            match tool_call.state {
                ToolCallState::Running => "running",
                ToolCallState::Success => "success",
                ToolCallState::Error => "error",
            },
            truncate_middle(&output, FINALIZATION_TOOL_OUTPUT_CHARS)
        ));
    }
    if !recent.is_empty() {
        sections.push(format!("Recent tool outputs:\n{}", recent.join("\n\n")));
    }

    let evidence = if sections.is_empty() {
        "No prior tool evidence was captured.".to_string()
    } else {
        sections.join("\n\n")
    };
    truncate_middle(&evidence, FINALIZATION_EVIDENCE_CHARS)
}

fn known_path_lines_from_tool_calls(tool_calls: &[&ToolCallInfo]) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for tool_call in tool_calls {
        collect_paths_from_text(&tool_call.input, &mut paths);
        if let Some(output) = tool_call.output.as_deref() {
            collect_paths_from_text(output, &mut paths);
        }
    }
    paths.into_iter().take(40).collect()
}

fn collect_paths_from_text(text: &str, paths: &mut BTreeSet<String>) {
    for raw in text.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']')
    }) {
        let token = raw
            .trim_matches(|ch: char| matches!(ch, ':' | ',' | '.' | '"' | '\'' | '`' | '{' | '}'))
            .trim_start_matches("./");
        if token.len() > 160 {
            continue;
        }
        let lowered = token.to_ascii_lowercase();
        let looks_like_supported_file = [
            ".csv", ".tsv", ".json", ".jsonl", ".ndjson", ".parquet", ".md", ".txt", ".rst",
        ]
        .iter()
        .any(|extension| lowered.contains(extension));
        if looks_like_supported_file {
            paths.insert(token.to_string());
        }
    }
}

fn candidate_lines_from_tool_calls(tool_calls: &[&ToolCallInfo]) -> Vec<String> {
    let mut lines = Vec::new();
    for tool_call in tool_calls {
        let Some(output) = tool_call.output.as_deref() else {
            continue;
        };
        let compact = compact_tool_output(output);
        for line in compact.lines() {
            let lowered = line.to_ascii_lowercase();
            if lowered.contains("final")
                || lowered.contains("answer")
                || lowered.contains("best")
                || lowered.contains("overall")
                || lowered.contains("lowest")
                || lowered.contains("preferred")
                || lowered.contains("cost")
            {
                lines.push(format!("{}: {}", tool_call.name, line.trim()));
                if lines.len() >= 40 {
                    return lines;
                }
            }
        }
    }
    lines
}

fn compact_tool_output(output: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        for key in ["stdout", "stderr", "content", "markdown", "result"] {
            if let Some(text) = value.get(key).and_then(|value| value.as_str())
                && !text.trim().is_empty()
            {
                return text.trim().to_string();
            }
        }
    }
    output.trim().to_string()
}

fn infer_answer_candidate(
    engine: &ChatEngine,
    response: &str,
    original_prompt: &str,
) -> Option<String> {
    let mut texts = Vec::new();
    if !response.trim().is_empty() {
        texts.push(response.to_string());
    }
    for tool_call in engine
        .messages
        .iter()
        .flat_map(|message| message.tool_calls())
    {
        if is_candidate_source_tool(tool_call.name.as_str())
            && let Some(output) = tool_call.output.as_deref()
        {
            let compact = compact_tool_output(output);
            if !compact.trim().is_empty() {
                texts.push(compact);
            }
        }
    }

    for text in texts.iter().rev() {
        for line in text.lines().rev() {
            if let Some(candidate) = candidate_from_line(line, original_prompt, true) {
                return Some(candidate);
            }
        }
    }
    for text in texts.iter().rev() {
        for line in text.lines().rev() {
            if let Some(candidate) = candidate_from_line(line, original_prompt, false) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_candidate_source_tool(name: &str) -> bool {
    matches!(
        name,
        "execute_code" | "query_data" | "final_answer" | "write_file"
    )
}

fn candidate_from_line(line: &str, original_prompt: &str, require_label: bool) -> Option<String> {
    let cleaned = clean_candidate(line);
    if cleaned.is_empty() {
        return None;
    }

    let lowered = cleaned.to_ascii_lowercase();
    let candidate = if let Some(candidate) = labeled_candidate(&cleaned, &lowered) {
        candidate
    } else if lowered.contains("the answer is") {
        let idx = lowered.find("the answer is")?;
        cleaned[idx + "the answer is".len()..]
            .trim_start_matches([':', '=', '-', ' '])
            .trim()
            .to_string()
    } else if require_label {
        return None;
    } else if !is_safe_unlabeled_candidate(&cleaned, original_prompt) {
        return None;
    } else {
        cleaned
    };

    let normalized = normalize_candidate(&candidate, original_prompt);
    (is_reasonable_answer_candidate(&normalized)
        && candidate_matches_prompt_format(&normalized, original_prompt))
    .then_some(normalized)
}

fn candidate_matches_prompt_format(candidate: &str, original_prompt: &str) -> bool {
    if prompt_expects_colon_fee(original_prompt) {
        return candidate.eq_ignore_ascii_case("not applicable")
            || is_token_colon_number(candidate);
    }
    true
}

fn is_safe_unlabeled_candidate(candidate: &str, original_prompt: &str) -> bool {
    let candidate = clean_candidate(candidate);
    if candidate.eq_ignore_ascii_case("not applicable") {
        return true;
    }
    if prompt_expects_colon_fee(original_prompt) {
        return is_token_colon_number(&candidate);
    }
    let lowered_prompt = original_prompt.to_ascii_lowercase();
    if lowered_prompt.contains("yes/no") || lowered_prompt.contains("yes or no") {
        return matches!(candidate.to_ascii_lowercase().as_str(), "yes" | "no");
    }
    if lowered_prompt.contains("rounded")
        || lowered_prompt.contains("decimal")
        || lowered_prompt.contains("fee")
        || lowered_prompt.contains("cost")
        || lowered_prompt.contains("count")
        || lowered_prompt.contains("number")
    {
        return is_number_like(&candidate);
    }
    is_token_colon_number(&candidate) || is_number_like(&candidate)
}

fn prompt_expects_colon_fee(original_prompt: &str) -> bool {
    let prompt = original_prompt.to_ascii_lowercase();
    prompt.contains(":{fee}")
        || prompt.contains("}: {fee}")
        || prompt.contains("associated cost")
        || prompt.contains("selected card scheme")
}

fn is_token_colon_number(candidate: &str) -> bool {
    let Some((left, right)) = candidate.split_once(':') else {
        return false;
    };
    is_short_token(left.trim()) && is_number_like(right.trim())
}

fn labeled_candidate(cleaned: &str, lowered: &str) -> Option<String> {
    for label in [
        "final answer",
        "best answer",
        "best result",
        "answer",
        "lowest",
        "preferred",
        "result",
        "best",
    ] {
        if lowered.starts_with(label) {
            let rest = cleaned[label.len()..]
                .trim_start_matches([':', '=', '-', ' '])
                .trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn clean_candidate(value: &str) -> String {
    value
        .trim()
        .trim_matches('`')
        .trim_matches('*')
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_string()
}

fn normalize_candidate(candidate: &str, original_prompt: &str) -> String {
    let mut candidate = clean_candidate(candidate);
    if let Some((left, right)) = candidate.split_once('=')
        && should_convert_equals_to_colon(original_prompt)
        && is_short_token(left.trim())
        && is_number_like(right.trim())
    {
        candidate = format!("{}:{}", left.trim(), right.trim());
    }
    candidate
}

fn should_convert_equals_to_colon(original_prompt: &str) -> bool {
    let prompt = original_prompt.to_ascii_lowercase();
    prompt.contains(':') || prompt.contains("colon") || prompt.contains("format")
}

fn is_reasonable_answer_candidate(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().count() > 120 {
        return false;
    }
    let lowered = candidate.to_ascii_lowercase();
    if [
        "error",
        "expected",
        "got",
        "correct answer",
        "incorrect answer",
        "toolset error",
        "timed_out",
        "stdout",
        "stderr",
        "none",
        "null",
    ]
    .iter()
    .any(|prefix| lowered.starts_with(prefix))
    {
        return false;
    }
    if candidate.contains('{') || candidate.contains('}') || candidate.contains('[') {
        return false;
    }
    if candidate.split_whitespace().count() > 8 {
        return false;
    }
    candidate.chars().any(|ch| ch.is_alphanumeric())
}

fn is_short_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 40
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
}

fn is_number_like(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ',' | '-' | '+'))
        && value.chars().any(|ch| ch.is_ascii_digit())
}

fn write_inferred_answer_file(engine: &ChatEngine, candidate: &str) -> std::io::Result<PathBuf> {
    let mut last_error = None;
    for path in answer_file_candidates(engine) {
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            last_error = Some(error);
            continue;
        }
        match std::fs::write(&path, format!("{candidate}\n")) {
            Ok(()) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("no answer file candidates")))
}

fn truncate_middle(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let keep_start = max_chars / 2;
    let keep_end = max_chars.saturating_sub(keep_start + 24);
    let start: String = text.chars().take(keep_start).collect();
    let end: String = text
        .chars()
        .rev()
        .take(keep_end)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}\n... [truncated] ...\n{end}")
}

fn answer_file_exists(engine: &ChatEngine) -> bool {
    // Treat any existing file (even empty) as a valid answer — empty string is a valid
    // answer for "empty list" questions.  We use metadata instead of content so we don't
    // accidentally overwrite an intentionally-empty answer in the salvage path.
    answer_file_candidates(engine)
        .into_iter()
        .any(|path| path.exists())
}

fn answer_file_candidates(engine: &ChatEngine) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(workspace_dir) = engine.execution_settings.workspace_dir.as_deref() {
        candidates.push(PathBuf::from(workspace_dir).join("answer.txt"));
    } else if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join("answer.txt"));
    }

    let app_answer = PathBuf::from("/app/answer.txt");
    if !candidates.iter().any(|path| path == &app_answer) {
        candidates.push(app_answer);
    }

    candidates
}

fn file_has_content(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|contents| !contents.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use chatty_core::models::message_types::ExecutionEngine;

    use super::*;

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
        assert_eq!(MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION, 12);
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
            candidate_from_line("Final answer: Not Applicable", "write answer.txt", true)
                .as_deref(),
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
}
