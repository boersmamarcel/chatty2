//! The summary a compaction replaces the older part of a history with.
//!
//! [`ContextShaper`](super::ContextShaper) decides *when* to compact and
//! *which* messages go; this module writes what takes their place. The
//! summary has two halves:
//!
//! - **Facts the harness guarantees**, read straight from the history and the
//!   workspace, never left to the model: the task verbatim (the first user
//!   message), the files the tool history wrote, a `git status --short`
//!   snapshot when the workspace is a git repository, and the tail of the last
//!   command's output.
//! - **Prose** — what was done, findings, open questions, next step — from one
//!   tool-free model call ([`Summarizer`]), with a deterministic fallback when
//!   that call fails, times out or comes back empty.
//!
//! Plain truncation is what lost small models the thread on long SWE-bench
//! runs: after a cut they debugged a pip-installed copy instead of the repo,
//! or read a coding task as a quiz. The summary therefore closes by telling
//! the model to re-check the workspace before it goes on.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use futures::future::BoxFuture;
use rig_core::completion::Message;
use rig_core::completion::message::{AssistantContent, Text, ToolResultContent};
use rig_core::message::UserContent;
use tracing::warn;

// ── Tunables ──────────────────────────────────────────────────────────────────

/// The task is carried verbatim up to this many characters; a longer one is
/// cut with a note (a task that size is a pasted document, not an ask).
const TASK_MAX_CHARS: usize = 12_000;

/// Characters of the last command's output the summary carries (its tail).
const LAST_COMMAND_TAIL_CHARS: usize = 2_000;

/// Lines of `git status --short` the summary carries.
const GIT_STATUS_MAX_LINES: usize = 60;

/// How long `git status` may take before the summary goes without it.
const GIT_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// Shell commands that may have written files, listed at most this many.
const SHELL_WRITES_MAX: usize = 12;

/// How long the summarising model call may take. Well under the stream stall
/// watchdog (180 s): the hook runs inside the turn, which emits nothing while
/// it waits.
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(90);

/// The model's prose is kept up to this many characters.
const SUMMARY_PROSE_MAX_CHARS: usize = 3_000;

/// Per-message caps when the compacted messages are rendered for the model.
const RENDER_TEXT_CHARS: usize = 1_500;
const RENDER_TOOL_ARGS_CHARS: usize = 300;
const RENDER_TOOL_RESULT_CHARS: usize = 600;

/// Tools that write a file, with the argument naming it.
const FILE_WRITE_TOOLS: &[(&str, &str)] = &[
    ("write_file", "path"),
    ("apply_diff", "path"),
    ("delete_file", "path"),
    ("move_file", "destination"),
    ("create_directory", "path"),
];

/// Tools that run a command; their output is the "last command result".
const COMMAND_TOOLS: &[&str] = &["shell_execute", "execute_code"];

/// Markers of a shell command that writes files.
const SHELL_WRITE_MARKERS: &[&str] = &[
    " > ",
    ">>",
    "sed -i",
    "tee ",
    "git apply",
    "patch ",
    "cat >",
    "mv ",
    "cp ",
    "rm ",
];

/// The heading the summary message opens with; also how a later compaction
/// recognises a summary in the history it is given.
pub const COMPACTION_HEADER: &str = "[CONTEXT COMPACTED]";

// ── The summariser seam ───────────────────────────────────────────────────────

/// One tool-free model call that turns a prompt into text: the agent's
/// utility agent in production, a scripted mock model in tests.
pub trait Summarizer: Send + Sync {
    fn summarize<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, anyhow::Result<String>>;
}

impl Summarizer for rig_agent::Agent {
    fn summarize<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, anyhow::Result<String>> {
        use rig_agent::completion::Prompt;
        Box::pin(async move { Ok(self.prompt(prompt.to_string()).await?) })
    }
}

// ── Building the summary ──────────────────────────────────────────────────────

/// Everything [`build_summary`] needs besides the messages.
pub struct SummaryInputs<'a> {
    /// The full history as recorded; `covered` is the prefix being replaced.
    pub history: &'a [Message],
    pub covered: usize,
    /// The prose of the compaction this one supersedes, if any.
    pub previous_prose: Option<&'a str>,
    pub workspace: Option<&'a PathBuf>,
    pub summarizer: Option<&'a dyn Summarizer>,
    /// The rendered transcript is cut to its last this-many characters, so
    /// the summarising call itself fits the model's window.
    pub transcript_max_chars: usize,
}

/// The summary message and its prose (kept so the next compaction can build
/// on it).
pub struct Summary {
    pub message: Message,
    pub prose: String,
}

/// Build the message that replaces `history[..covered]`.
pub async fn build_summary(inputs: SummaryInputs<'_>) -> Summary {
    let history = inputs.history;
    let covered = &history[..inputs.covered];
    let task = original_task(history);
    let writes = file_writes(covered);
    let git_status = match inputs.workspace {
        Some(dir) => git_status_short(dir).await,
        None => None,
    };
    let last_command = last_command_result(covered);

    let prose = match inputs.summarizer {
        Some(summarizer) => {
            let prompt = summary_prompt(
                task.as_deref().unwrap_or("(no task text found)"),
                inputs.previous_prose,
                &render_transcript(covered, inputs.transcript_max_chars),
            );
            match tokio::time::timeout(SUMMARY_TIMEOUT, summarizer.summarize(&prompt)).await {
                Ok(Ok(text)) => clean_prose(&text),
                Ok(Err(e)) => {
                    warn!(error = %e, "compaction: summary call failed; using the deterministic summary");
                    None
                }
                Err(_) => {
                    warn!(
                        timeout_secs = SUMMARY_TIMEOUT.as_secs(),
                        "compaction: summary call timed out; using the deterministic summary"
                    );
                    None
                }
            }
        }
        None => None,
    };
    let prose = prose.unwrap_or_else(|| deterministic_prose(covered, inputs.previous_prose));

    let text = render_summary(
        inputs.covered,
        task.as_deref(),
        &writes,
        git_status.as_deref(),
        last_command.as_ref(),
        &prose,
    );
    Summary {
        message: Message::User {
            content: vec![UserContent::Text(Text::new(text))],
        },
        prose,
    }
}

fn render_summary(
    replaced: usize,
    task: Option<&str>,
    writes: &FileWrites,
    git_status: Option<&str>,
    last_command: Option<&(String, String)>,
    prose: &str,
) -> String {
    let mut out = format!(
        "{COMPACTION_HEADER} The {replaced} earliest messages of this run were replaced by this \
         summary to stay inside the model's context window. Everything after it is verbatim.\n\n"
    );

    out.push_str("## Task (the original request, verbatim)\n");
    match task {
        Some(task) => {
            out.push_str(&head_chars(task, TASK_MAX_CHARS));
            if task.chars().count() > TASK_MAX_CHARS {
                out.push_str("\n[… task text cut at 12 000 characters]");
            }
        }
        None => out.push_str("(no task text found in the history)"),
    }

    out.push_str("\n\n## Files changed so far\n");
    if writes.files.is_empty() && writes.shell.is_empty() {
        out.push_str("No file-writing tool call in the compacted part.\n");
    }
    for (tool, path) in &writes.files {
        out.push_str(&format!("- {path} ({tool})\n"));
    }
    if !writes.shell.is_empty() {
        out.push_str("Shell commands that may have written files:\n");
        for command in &writes.shell {
            out.push_str(&format!("- `{command}`\n"));
        }
    }
    if let Some(status) = git_status {
        out.push_str("`git status --short` now:\n```\n");
        out.push_str(if status.is_empty() { "(clean)" } else { status });
        out.push_str("\n```\n");
    }

    out.push_str("\n## Last command result\n");
    match last_command {
        Some((command, output)) => {
            out.push_str(&format!("`{command}`\n```\n{output}\n```\n"));
        }
        None => out.push_str("No command was run in the compacted part.\n"),
    }

    out.push_str("\n## Progress, open questions and next step\n");
    out.push_str(prose.trim());

    out.push_str(
        "\n\n## Before you continue\nThe workspace is the source of truth, not this summary: \
         re-check the current state first (in a git repository, `git status && git diff`), \
         then carry on with the task above.",
    );
    out
}

/// The run's task: the text of the first user message that is not a tool
/// result and not an earlier compaction summary.
pub fn original_task(history: &[Message]) -> Option<String> {
    history.iter().find_map(|message| {
        let Message::User { content } = message else {
            return None;
        };
        let text: Vec<&str> = content
            .iter()
            .filter_map(|item| match item {
                UserContent::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        let text = text.join("\n");
        (!text.trim().is_empty() && !text.starts_with(COMPACTION_HEADER)).then_some(text)
    })
}

/// What the tool history says was written.
#[derive(Debug, Default, PartialEq)]
pub struct FileWrites {
    /// `(tool, path)`, first write first, each path once.
    pub files: Vec<(String, String)>,
    /// Shell commands that look like they write files, most recent last.
    pub shell: Vec<String>,
}

pub fn file_writes(messages: &[Message]) -> FileWrites {
    let mut writes = FileWrites::default();
    let mut seen = BTreeSet::new();
    for (name, args) in tool_calls(messages) {
        if let Some((_, key)) = FILE_WRITE_TOOLS.iter().find(|(tool, _)| *tool == name) {
            if let Some(path) = args.get(*key).and_then(|v| v.as_str())
                && seen.insert(path.to_string())
            {
                writes.files.push((name.to_string(), path.to_string()));
            }
        } else if name == "shell_execute"
            && let Some(command) = args.get("command").and_then(|v| v.as_str())
            && SHELL_WRITE_MARKERS.iter().any(|m| command.contains(m))
        {
            writes
                .shell
                .push(head_chars(command.trim(), 160).replace('\n', " "));
        }
    }
    let excess = writes.shell.len().saturating_sub(SHELL_WRITES_MAX);
    writes.shell.drain(..excess);
    writes
}

fn tool_calls(messages: &[Message]) -> impl Iterator<Item = (&str, &serde_json::Value)> {
    messages.iter().flat_map(|message| match message {
        Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(call) => {
                    Some((call.function.name.as_str(), &call.function.arguments))
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    })
}

/// `(command, tail of output)` for the last command tool whose result is in
/// `messages`.
pub fn last_command_result(messages: &[Message]) -> Option<(String, String)> {
    let mut commands = std::collections::HashMap::new();
    let mut last = None;
    for message in messages {
        match message {
            Message::Assistant { content, .. } => {
                for item in content {
                    if let AssistantContent::ToolCall(call) = item
                        && COMMAND_TOOLS.contains(&call.function.name.as_str())
                    {
                        let args = &call.function.arguments;
                        let command = args
                            .get("command")
                            .or_else(|| args.get("code"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| args.to_string());
                        commands.insert(call.id.clone(), command);
                    }
                }
            }
            Message::User { content } => {
                for item in content {
                    if let UserContent::ToolResult(result) = item
                        && let Some(command) = commands.get(&result.call)
                    {
                        last = Some((command.clone(), result_text(&result.content)));
                    }
                }
            }
            Message::System { .. } => {}
        }
    }
    last.map(|(command, output)| {
        (
            head_chars(command.trim(), 300).replace('\n', " "),
            tail_chars(output.trim(), LAST_COMMAND_TAIL_CHARS),
        )
    })
}

fn result_text(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            ToolResultContent::Text(text) => text.text.clone(),
            ToolResultContent::Json { value } => match value.as_str() {
                Some(s) => s.to_string(),
                None => value.to_string(),
            },
            ToolResultContent::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `git status --short` in `dir`, or `None` when it is not a git repository
/// (or git is missing, or slow).
pub async fn git_status_short(dir: &PathBuf) -> Option<String> {
    let run = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--short"])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(GIT_STATUS_TIMEOUT, run)
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = lines
        .iter()
        .take(GIT_STATUS_MAX_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > GIT_STATUS_MAX_LINES {
        kept.push_str(&format!("\n… {} more", lines.len() - GIT_STATUS_MAX_LINES));
    }
    Some(kept)
}

// ── The model's part ──────────────────────────────────────────────────────────

fn summary_prompt(task: &str, previous: Option<&str>, transcript: &str) -> String {
    let mut prompt = String::from(
        "You are compacting the working history of a coding agent so it can continue with less \
         context. Write a progress note of at most 250 words under exactly these headings: \
         'Done so far', 'Findings', 'Open questions', 'Next step'. Be concrete: name files, \
         functions, commands and error messages. Do not solve the task and do not call tools.\n\n",
    );
    prompt.push_str("## Task\n");
    prompt.push_str(&head_chars(task, 4_000));
    if let Some(previous) = previous {
        prompt.push_str("\n\n## Summary of the work before this transcript\n");
        prompt.push_str(previous);
    }
    prompt.push_str("\n\n## Transcript\n");
    prompt.push_str(transcript);
    prompt
}

/// The model's reply with any `<think>` block dropped and cut to size;
/// `None` when nothing is left.
fn clean_prose(text: &str) -> Option<String> {
    let text = match text.rfind("</think>") {
        Some(end) => &text[end + "</think>".len()..],
        None => text,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| head_chars(text, SUMMARY_PROSE_MAX_CHARS))
}

/// The fallback prose: which tools were used, and the last thing the model
/// said it would do.
fn deterministic_prose(covered: &[Message], previous: Option<&str>) -> String {
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for (name, _) in tool_calls(covered) {
        *counts.entry(name).or_default() += 1;
    }
    let mut out = String::from("(Model summary unavailable; built from the tool history.)\n");
    if let Some(previous) = previous {
        out.push_str("Earlier summary:\n");
        out.push_str(previous.trim());
        out.push('\n');
    }
    if !counts.is_empty() {
        let used: Vec<String> = counts.iter().map(|(n, c)| format!("{n} ×{c}")).collect();
        out.push_str(&format!("Done so far: tool calls {}.\n", used.join(", ")));
    }
    let last_note = covered.iter().rev().find_map(|message| match message {
        Message::Assistant { content, .. } => {
            let text: Vec<&str> = content
                .iter()
                .filter_map(|item| match item {
                    AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                        Some(text.text.as_str())
                    }
                    _ => None,
                })
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        _ => None,
    });
    match last_note {
        Some(note) => out.push_str(&format!(
            "Last note from the assistant: {}\n",
            tail_chars(note.trim(), 1_200)
        )),
        None => out.push_str("No assistant notes in the compacted part.\n"),
    }
    out.push_str(
        "Open questions: unknown — check the workspace.\nNext step: continue the task from the \
         current state of the workspace.",
    );
    out
}

/// The compacted messages as plain text for the summarising call, cut to
/// their last `max_chars` characters.
fn render_transcript(messages: &[Message], max_chars: usize) -> String {
    let mut names = std::collections::HashMap::new();
    let mut out = String::new();
    for message in messages {
        match message {
            Message::System { content } => {
                out.push_str(&format!(
                    "SYSTEM: {}\n",
                    head_chars(content, RENDER_TEXT_CHARS)
                ));
            }
            Message::User { content } => {
                for item in content {
                    match item {
                        UserContent::Text(text) => out.push_str(&format!(
                            "USER: {}\n",
                            head_chars(&text.text, RENDER_TEXT_CHARS)
                        )),
                        UserContent::ToolResult(result) => {
                            let name = names
                                .get(&result.call)
                                .cloned()
                                .unwrap_or_else(|| result.name.clone());
                            out.push_str(&format!(
                                "RESULT {name}: {}\n",
                                head_tail_chars(
                                    &result_text(&result.content),
                                    RENDER_TOOL_RESULT_CHARS
                                )
                            ));
                        }
                        _ => out.push_str("USER: [attachment]\n"),
                    }
                }
            }
            Message::Assistant { content, .. } => {
                for item in content {
                    match item {
                        AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                            out.push_str(&format!(
                                "ASSISTANT: {}\n",
                                head_chars(&text.text, RENDER_TEXT_CHARS)
                            ));
                        }
                        AssistantContent::ToolCall(call) => {
                            names.insert(call.id.clone(), call.function.name.clone());
                            out.push_str(&format!(
                                "CALL {}({})\n",
                                call.function.name,
                                head_chars(
                                    &call.function.arguments.to_string(),
                                    RENDER_TOOL_ARGS_CHARS
                                )
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let total = out.chars().count();
    if total > max_chars {
        format!(
            "[… {} earlier characters omitted]\n{}",
            total - max_chars,
            tail_chars(&out, max_chars)
        )
    } else {
        out
    }
}

// ── Char helpers ──────────────────────────────────────────────────────────────

fn head_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn tail_chars(text: &str, n: usize) -> String {
    let total = text.chars().count();
    text.chars().skip(total.saturating_sub(n)).collect()
}

fn head_tail_chars(text: &str, n: usize) -> String {
    let total = text.chars().count();
    if total <= n {
        return text.to_string();
    }
    format!(
        "{} […] {}",
        head_chars(text, n / 2),
        tail_chars(text, n / 2)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, args: serde_json::Value) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::tool_call(id, name, args)],
        }
    }

    fn result(id: &str, text: &str) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(rig_core::message::ToolResult {
                call: rig_core::message::ToolCallId::new(id).unwrap(),
                provider: None,
                name: "t".into(),
                content: vec![ToolResultContent::Text(Text::new(text.to_string()))],
            })],
        }
    }

    #[test]
    fn file_writes_come_from_write_tools_and_writing_shell_commands() {
        let history = vec![
            call("1", "write_file", serde_json::json!({"path": "src/a.py"})),
            result("1", "ok"),
            call("2", "apply_diff", serde_json::json!({"path": "src/a.py"})),
            result("2", "ok"),
            call(
                "3",
                "shell_execute",
                serde_json::json!({"command": "ls src"}),
            ),
            result("3", "a.py"),
            call(
                "4",
                "shell_execute",
                serde_json::json!({"command": "sed -i 's/x/y/' src/b.py"}),
            ),
            result("4", ""),
        ];
        let writes = file_writes(&history);
        assert_eq!(
            writes.files,
            vec![("write_file".to_string(), "src/a.py".to_string())]
        );
        assert_eq!(writes.shell, vec!["sed -i 's/x/y/' src/b.py".to_string()]);
    }

    #[test]
    fn the_last_command_result_is_its_tail() {
        let long = format!("{}FAILED test_x", "x".repeat(5_000));
        let history = vec![
            call(
                "1",
                "shell_execute",
                serde_json::json!({"command": "pytest -q"}),
            ),
            result("1", &long),
            call("2", "read_file", serde_json::json!({"path": "a"})),
            result("2", "file"),
        ];
        let (command, output) = last_command_result(&history).expect("a command ran");
        assert_eq!(command, "pytest -q");
        assert!(output.ends_with("FAILED test_x"));
        assert_eq!(output.chars().count(), LAST_COMMAND_TAIL_CHARS);
    }

    #[test]
    fn the_task_skips_earlier_summaries_and_tool_results() {
        let history = vec![
            Message::user(format!("{COMPACTION_HEADER} old summary")),
            Message::user("fix the bug in parser.py"),
        ];
        assert_eq!(
            original_task(&history).as_deref(),
            Some("fix the bug in parser.py")
        );
    }

    #[test]
    fn think_blocks_are_dropped_from_the_prose() {
        assert_eq!(
            clean_prose("<think>hmm</think>\nDone so far: x").as_deref(),
            Some("Done so far: x")
        );
        assert_eq!(clean_prose("<think>only thinking</think>  "), None);
    }

    #[tokio::test]
    async fn git_status_reports_a_repository_and_skips_a_plain_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        assert_eq!(git_status_short(&path).await, None, "not a repository");

        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return; // no git on this host
        }
        std::fs::write(path.join("new.py"), "x").unwrap();
        assert_eq!(git_status_short(&path).await.as_deref(), Some("?? new.py"));
    }
}
