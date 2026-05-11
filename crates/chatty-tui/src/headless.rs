use anyhow::Result;
use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use crate::engine::{ChatEngine, ToolCallInfo, ToolCallState};
use crate::events::AppEvent;

const MAX_STREAM_ERROR_RECOVERY_ATTEMPTS: usize = 2;
const STREAM_ERROR_RECOVERY_PROMPT: &str = "The previous provider response failed to parse after partial progress. Continue from the existing conversation and prior tool results. Do not repeat earlier analysis. If the answer is ready, write /app/answer.txt now. Otherwise use at most one compact tool call and keep any execute_code output short.";

/// Run in headless mode: send a message, collect the response, print to stdout.
pub async fn run_headless(
    mut engine: ChatEngine,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    message: String,
) -> Result<()> {
    // Send message
    engine.send_message(message);

    // Collect response
    let mut response = String::new();
    let mut recovery_attempts = 0usize;

    while let Some(event) = event_rx.recv().await {
        match event {
            AppEvent::TextChunk(text) => {
                engine.handle_event(AppEvent::TextChunk(text.clone()));
                // Stream text chunks to stderr so parent process can show live progress.
                eprint!("{}", text);
                response.push_str(&text);
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
            }
            AppEvent::StreamCompleted => {
                engine.handle_event(AppEvent::StreamCompleted);
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
                    eprintln!(
                        "Retrying after stream error ({}/{}) with a compact continuation prompt.",
                        recovery_attempts, MAX_STREAM_ERROR_RECOVERY_ATTEMPTS
                    );
                    engine.send_message(STREAM_ERROR_RECOVERY_PROMPT.to_string());
                    continue;
                }

                break;
            }
            AppEvent::StreamCancelled => {
                engine.handle_event(AppEvent::StreamCancelled);
                break;
            }
            // Handle other events silently
            _ => {
                engine.handle_event(event);
            }
        }
    }

    // Print response to stdout
    println!("{}", response);

    Ok(())
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
}

fn answer_file_exists(engine: &ChatEngine) -> bool {
    answer_file_candidates(engine)
        .into_iter()
        .any(|path| file_has_content(&path))
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
        assert!(!is_retryable_stream_error("network timeout"));
    }
}
