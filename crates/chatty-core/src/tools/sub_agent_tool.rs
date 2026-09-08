use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::models::message_types::ToolSource;
use crate::services::git_service::GitService;
use crate::session::SessionEvent;
use crate::tools::ToolError;
use crate::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};

/// Prefix for a headless child's turn events on stderr: one
/// [`SessionEvent`] per line, JSON-encoded (AGE-196). This is the session's
/// own contract crossing the process boundary, so the parent binds to typed
/// events rather than a text protocol. Every other stderr line is the
/// child's human-readable log.
///
/// Not every event is written: `Text` (the answer is the child's stdout) and
/// `TurnMessages` (persistence-only, and large) stay in the child. See
/// [`format_event_line`].
pub const CHATTY_EVENT_PREFIX: &str = "CHATTY_EVENT\t";

/// The `CHATTY_EVENT` line for `event`, or `None` for the events that stay
/// in the child (see [`CHATTY_EVENT_PREFIX`]).
pub fn format_event_line(event: &SessionEvent) -> Option<String> {
    if matches!(event, SessionEvent::Text(_) | SessionEvent::TurnMessages(_)) {
        return None;
    }
    let json = serde_json::to_string(event)
        .map_err(|e| warn!(error = ?e, "Failed to serialize a session event for the parent"))
        .ok()?;
    Some(format!("{CHATTY_EVENT_PREFIX}{json}"))
}

/// Parse a `CHATTY_EVENT` line back into the event. `None` for any other
/// line, including a malformed one (logged, not fatal: the parent keeps
/// draining).
pub fn parse_event_line(line: &str) -> Option<SessionEvent> {
    let json = line.strip_prefix(CHATTY_EVENT_PREFIX)?;
    serde_json::from_str(json)
        .map_err(|e| warn!(error = ?e, "Ignoring a malformed CHATTY_EVENT line from a sub-agent"))
        .ok()
}

/// Compact progress text for the parent UI from a child's event: `name` on
/// a tool start, `✓ name` / `✗ name` on its result, and the child's stream
/// error. `names` remembers each tool call's name until its result arrives.
fn progress_text_for_event(
    event: &SessionEvent,
    names: &mut HashMap<String, String>,
) -> Option<String> {
    match event {
        SessionEvent::ToolCallStarted { id, name } => {
            names.insert(id.clone(), name.clone());
            Some(name.clone())
        }
        SessionEvent::ToolCallResult { id, .. } => {
            names.remove(id).map(|name| format!("\u{2713} {name}"))
        }
        SessionEvent::ToolCallError { id, .. } => {
            names.remove(id).map(|name| format!("\u{2717} {name}"))
        }
        SessionEvent::Error(error) => Some(format!("error: {}", error.message)),
        _ => None,
    }
}

/// Maximum number of characters from stderr to include in error messages.
const STDERR_PREVIEW_CHARS: usize = 500;

/// Arguments for the sub_agent tool
#[derive(Deserialize, Serialize)]
pub struct SubAgentArgs {
    /// The task or prompt to delegate to the sub-agent.
    pub task: String,
    /// Optional model ID to use for the sub-agent. If omitted, the parent's
    /// model is used. Use `list_tools` to see the current model or check
    /// the available model IDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Output from the sub_agent tool
#[derive(Debug, Serialize)]
pub struct SubAgentOutput {
    /// The sub-agent's response text.
    pub response: String,
    /// Whether the sub-agent completed successfully.
    pub success: bool,
    /// Branch holding the worker's committed output, when it ran in its own
    /// worktree. The leader merges this; it is not merged automatically.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Path of the worker's worktree, left in place for inspection and merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

/// Tool that spawns a sub-agent (chatty-tui in headless mode) to handle a
/// delegated task autonomously.
///
/// The master agent can use this tool to spin up independent sub-agents that
/// have access to the same tool set. Each sub-agent runs in its own process,
/// executes the task, and returns the result. This enables the master agent
/// to parallelize work by launching multiple sub-agents for different tasks.
///
/// The sub-agent may optionally use a different model than its parent.
#[derive(Clone)]
pub struct SubAgentTool {
    /// Model ID the sub-agent uses by default (inherits from the parent conversation).
    model_id: String,
    /// Whether to auto-approve tool calls in the sub-agent.
    auto_approve: bool,
    /// Available model IDs for validation (empty = skip validation).
    available_model_ids: Vec<String>,
    /// Shared slot with `InvokeAgentTool` for compact live progress in the parent UI.
    progress_slot: InvokeAgentProgressSlot,
    /// Workspace root the parent is confined to. A worker gets a `git worktree`
    /// branched from it (AGE-314); without it, or when it is not a git
    /// repository, workers share the parent's tree as they did before.
    workspace_dir: Option<String>,
}

impl SubAgentTool {
    pub fn new(
        model_id: String,
        auto_approve: bool,
        available_model_ids: Vec<String>,
        progress_slot: InvokeAgentProgressSlot,
        workspace_dir: Option<String>,
    ) -> Self {
        Self {
            model_id,
            auto_approve,
            available_model_ids,
            progress_slot,
            workspace_dir,
        }
    }

    fn send_progress(&self, event: InvokeAgentProgress) {
        send_progress(&self.progress_slot, event);
    }
}

/// Returns true when `line` is a headless child's `CHATTY_EVENT` line.
pub fn is_chatty_event_line(line: &str) -> bool {
    line.starts_with(CHATTY_EVENT_PREFIX)
}

fn send_progress(slot: &InvokeAgentProgressSlot, event: InvokeAgentProgress) {
    let guard = slot.lock();
    if let Some(tx) = guard.as_ref() {
        let _ = tx.send(event);
    }
}

impl Tool for SubAgentTool {
    const NAME: &'static str = "sub_agent";
    type Error = ToolError;
    type Args = SubAgentArgs;
    type Output = SubAgentOutput;

    fn description(&self) -> String {
        "Delegate a task to an independent sub-agent that has access to the \
                         same tools as you. The sub-agent runs autonomously in its own process, \
                         executes the task (including any tool calls it needs), and returns the \
                         result. Use this to parallelize work or to isolate complex sub-tasks. \
                         Each sub-agent starts with a fresh conversation context — provide all \
                         necessary context in the task description. \
                         You can optionally specify a different model for the sub-agent."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A detailed description of the task for the sub-agent. \
                                   Include all context the sub-agent needs since it does not \
                                   share conversation history with the parent."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model ID to use for the sub-agent. If omitted, \
                                   the sub-agent uses the same model as the parent. Use this \
                                   to pick a faster/cheaper model for simple tasks or a more \
                                   capable model for complex ones."
                }
            },
            "required": ["task"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let task = args.task.trim().to_string();
        if task.is_empty() {
            return Err(ToolError::OperationFailed(
                "Task description cannot be empty".to_string(),
            ));
        }

        // Resolve model: use the requested model if provided, else fall back
        // to the parent's model.
        let model_id = if let Some(requested) = &args.model {
            let requested = requested.trim().to_string();
            if requested.is_empty() {
                self.model_id.clone()
            } else if !self.available_model_ids.is_empty()
                && !self.available_model_ids.contains(&requested)
            {
                return Err(ToolError::OperationFailed(format!(
                    "Unknown model '{}'. Available models: {}",
                    requested,
                    self.available_model_ids.join(", ")
                )));
            } else {
                requested
            }
        } else {
            self.model_id.clone()
        };

        info!(
            task_len = task.len(),
            model = %model_id,
            "Launching sub-agent for delegated task"
        );

        // Find the chatty-tui binary: check same directory as current binary first,
        // then fall back to PATH resolution (may fail at spawn time if not found).
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("chatty-tui")))
            .filter(|p| p.exists())
            .unwrap_or_else(|| {
                warn!("chatty-tui not found next to current binary, falling back to PATH");
                PathBuf::from("chatty-tui")
            });

        let auto_approve = self.auto_approve;
        let progress_slot = self.progress_slot.clone();

        // AGE-314 / ADR-0012: give the worker its own worktree so two workers
        // editing one file produce a git conflict the leader can see, rather
        // than a last-writer-wins result neither of them reported.
        let worker = self.prepare_worktree().await;
        let workspace = worker.as_ref().map(|w| w.path.clone());

        // Run the subprocess in a blocking task to avoid blocking the async runtime.
        let result = tokio::task::spawn_blocking(move || {
            run_sub_agent_with_progress(exe, model_id, task, auto_approve, workspace, progress_slot)
        })
        .await
        .map_err(|e| {
            self.send_progress(InvokeAgentProgress::Finished {
                success: false,
                result: Some(e.to_string()),
            });
            ToolError::OperationFailed(format!("Sub-agent task failed to complete: {e}"))
        })?;

        // The worker's turn is not over until its output is durable on its
        // branch. This runs whether the child succeeded or failed: a failed
        // worker's partial edits are still the only copy that exists.
        let (branch, worktree) = match &worker {
            Some(w) => {
                self.commit_worker_output(w, &args.task).await;
                (Some(w.branch.clone()), Some(w.path.clone()))
            }
            None => (None, None),
        };

        let merge_hint = branch.as_ref().map(|b| {
            format!(
                "\n\n[Worker output is on branch '{b}'. Merge it to take the changes;                  its worktree is left in place until then.]"
            )
        });

        match result {
            Ok(stdout) => {
                let mut response = stdout.trim().to_string();
                if response.is_empty() {
                    response = "Sub-agent completed with no output.".to_string();
                }
                if let Some(hint) = merge_hint {
                    response.push_str(&hint);
                }
                Ok(SubAgentOutput {
                    response,
                    success: true,
                    branch,
                    worktree,
                })
            }
            Err(e) => Ok(SubAgentOutput {
                response: format!("Sub-agent failed: {e}"),
                success: false,
                branch,
                worktree,
            }),
        }
    }
}

/// A worker's isolated copy: a `git worktree` on its own branch.
struct WorkerWorktree {
    /// Worktree directory name under `WORKTREE_DIR`.
    name: String,
    /// Branch created for it, and where its output is committed.
    branch: String,
    /// Absolute path handed to the child as `--workspace`.
    path: String,
}

/// Monotonic suffix so two workers spawned in the same instant cannot collide.
static WORKER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl SubAgentTool {
    /// Create this worker's worktree, or `None` to fall back to the shared tree.
    ///
    /// Falling back is deliberate but lossy: without a git repository there is
    /// no branch to hand back and no conflict to surface, so parallel workers
    /// collide exactly as they did before. AGE-314 carries the open question of
    /// what a non-git workspace should do instead.
    async fn prepare_worktree(&self) -> Option<WorkerWorktree> {
        let root = self.workspace_dir.as_deref()?;

        let git = match GitService::new(root).await {
            Ok(git) => git,
            Err(e) => {
                warn!(
                    workspace = %root,
                    error = %e,
                    "No worktree isolation: workspace is not a git repository, \
                     so parallel sub-agents share one tree"
                );
                return None;
            }
        };

        let seq = WORKER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let name = format!("w{stamp}-{seq}");
        let branch = format!("sub-agent/{name}");

        match git.worktree_add(&name, &branch).await {
            Ok(path) => {
                info!(worktree = %path.display(), branch = %branch, "Sub-agent isolated in a worktree");
                Some(WorkerWorktree {
                    name,
                    branch,
                    path: path.to_string_lossy().to_string(),
                })
            }
            Err(e) => {
                warn!(error = %e, "No worktree isolation: falling back to the shared tree");
                None
            }
        }
    }

    /// Commit whatever the worker left behind onto its branch.
    ///
    /// Failures are logged, never fatal: the worker's answer is still worth
    /// returning, and the worktree stays on disk either way, so nothing the
    /// child wrote is destroyed by a failure here.
    async fn commit_worker_output(&self, worker: &WorkerWorktree, task: &str) {
        let git = match GitService::new(&worker.path).await {
            Ok(git) => git,
            Err(e) => {
                warn!(worktree = %worker.name, error = %e, "Cannot open the worker's worktree to commit it");
                return;
            }
        };

        let summary: String = task.chars().take(72).collect();
        let message = format!("sub-agent {}: {}", worker.name, summary);

        match git.commit_all(&message).await {
            Ok(Some(commit)) => {
                info!(branch = %worker.branch, hash = %commit.hash, "Worker output committed")
            }
            Ok(None) => info!(branch = %worker.branch, "Worker made no changes"),
            Err(e) => warn!(branch = %worker.branch, error = %e, "Failed to commit worker output"),
        }
    }
}

/// Emit Started/Finished around the child process so the parent UI row
/// never sticks on Running.
fn run_sub_agent_with_progress(
    executable: PathBuf,
    model_id: String,
    task: String,
    auto_approve: bool,
    workspace: Option<String>,
    progress_slot: InvokeAgentProgressSlot,
) -> Result<String, String> {
    send_progress(
        &progress_slot,
        InvokeAgentProgress::Started {
            agent_name: "sub_agent".to_string(),
            prompt: task.clone(),
            source: ToolSource::Local,
        },
    );
    let result = run_sub_agent(
        executable,
        model_id,
        task,
        auto_approve,
        workspace,
        progress_slot.clone(),
    );
    match &result {
        Ok(stdout) => send_progress(
            &progress_slot,
            InvokeAgentProgress::Finished {
                success: true,
                result: Some(stdout.clone()),
            },
        ),
        Err(e) => send_progress(
            &progress_slot,
            InvokeAgentProgress::Finished {
                success: false,
                result: Some(e.clone()),
            },
        ),
    }
    result
}

/// Spawn chatty-tui in headless mode and collect its output.
///
/// Stderr is drained live: the child's `CHATTY_EVENT` lines (its
/// `SessionEvent`s) are forwarded to the parent UI as compact progress.
/// stdout is returned as the tool result for the parent model.
fn run_sub_agent(
    executable: PathBuf,
    model_id: String,
    task: String,
    auto_approve: bool,
    workspace: Option<String>,
    progress_slot: InvokeAgentProgressSlot,
) -> Result<String, String> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(&executable);
    cmd.arg("--headless")
        .arg("--model")
        .arg(&model_id)
        .arg("--message")
        .arg(&task)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "macos")]
    if let Some(exe_dir) = executable.parent() {
        let frameworks_dir = exe_dir.join("../Frameworks");
        if frameworks_dir.join("libpdfium.dylib").exists() {
            cmd.env("CHATTY_PDFIUM_LIB_DIR", frameworks_dir);
        }
    }

    if auto_approve {
        cmd.arg("--auto-approve");
    }

    // Without this the child reads workspace_dir from the settings file it
    // shares with its parent, so every worker resolves to the same tree.
    if let Some(workspace) = &workspace {
        cmd.arg("--workspace").arg(workspace);
    }

    info!(exe = ?executable, "Launching headless sub-agent");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to launch sub-agent: {e}")),
    };

    let stderr = child.stderr.take();
    let slot_for_drain = progress_slot.clone();
    let stderr_thread = std::thread::spawn(move || {
        let mut collected = String::new();
        let mut tool_names = HashMap::new();
        if let Some(stderr) = stderr {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !is_chatty_event_line(&line) {
                    info!(sub_agent_progress = %line);
                    if !collected.is_empty() {
                        collected.push('\n');
                    }
                    collected.push_str(&line);
                }
                if let Some(event) = parse_event_line(&line)
                    && let Some(text) = progress_text_for_event(&event, &mut tool_names)
                {
                    send_progress(&slot_for_drain, InvokeAgentProgress::Text(text));
                }
            }
        }
        collected
    });

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            let _ = stderr_thread.join();
            return Err(format!("Sub-agent process failed: {e}"));
        }
    };

    let stderr_str = stderr_thread.join().unwrap_or_default();

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let exit_code = output.status.code();
        let stderr_preview = stderr_str
            .chars()
            .take(STDERR_PREVIEW_CHARS)
            .collect::<String>();
        Err(format!(
            "Sub-agent failed (exit {:?}): {}",
            exit_code, stderr_preview
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::install_progress_channel;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn dummy_slot() -> InvokeAgentProgressSlot {
        Arc::new(Mutex::new(None))
    }

    #[test]
    fn event_lines_round_trip_and_skip_the_child_only_events() {
        let started = SessionEvent::ToolCallStarted {
            id: "call-1".into(),
            name: "read_file".into(),
        };
        let line = format_event_line(&started).expect("tool events cross the boundary");
        assert!(is_chatty_event_line(&line));
        assert!(matches!(
            parse_event_line(&line),
            Some(SessionEvent::ToolCallStarted { id, name }) if id == "call-1" && name == "read_file"
        ));

        assert!(format_event_line(&SessionEvent::Text("token".into())).is_none());
        assert!(format_event_line(&SessionEvent::TurnMessages(Vec::new())).is_none());
        assert!(parse_event_line("CHATTY_EVENT\tnot json").is_none());
        assert!(parse_event_line("  [tool: read_file] running").is_none());
        assert!(!is_chatty_event_line("hello"));
    }

    #[test]
    fn progress_text_names_the_tool_and_its_outcome() {
        let mut names = HashMap::new();
        let started = SessionEvent::ToolCallStarted {
            id: "c".into(),
            name: "read_file".into(),
        };
        assert_eq!(
            progress_text_for_event(&started, &mut names).as_deref(),
            Some("read_file")
        );
        let ok = SessionEvent::ToolCallResult {
            id: "c".into(),
            result: "…".into(),
        };
        assert_eq!(
            progress_text_for_event(&ok, &mut names).as_deref(),
            Some("✓ read_file")
        );
        assert!(names.is_empty(), "the name is released with its result");
        assert_eq!(
            progress_text_for_event(&SessionEvent::TurnEnded, &mut names),
            None
        );
    }

    /// Two children at once, each on its own progress slot (AGE-196
    /// acceptance): every parent-side row sees its own child's events and
    /// nothing from the other's.
    #[cfg(unix)]
    #[test]
    fn two_concurrent_children_report_to_their_own_rows() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let script = dir.path().join("fake-tui");
        // The child names its tool after the task, so the parent can tell
        // whose events it received. `$5` is the task (`--headless --model M
        // --message TASK`).
        let started = format_event_line(&SessionEvent::ToolCallStarted {
            id: "c1".into(),
            name: "TASK".into(),
        })
        .unwrap()
        .replace("TASK", "'\"$5\"'");
        let finished = format_event_line(&SessionEvent::ToolCallResult {
            id: "c1".into(),
            result: "ok".into(),
        })
        .unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 sleep 0.2\n\
                 echo '{started}' >&2\n\
                 echo '{finished}' >&2\n\
                 echo \"done: $5\"\n"
            ),
        )
        .expect("write fixture");
        let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");

        let mut children = Vec::new();
        let mut receivers = Vec::new();
        for task in ["alpha", "beta"] {
            let slot = dummy_slot();
            receivers.push((task, install_progress_channel(&slot)));
            let script = script.clone();
            children.push(std::thread::spawn(move || {
                run_sub_agent_with_progress(
                    script,
                    "model-1".into(),
                    task.into(),
                    false,
                    None,
                    slot,
                )
            }));
        }
        let outputs: Vec<String> = children
            .into_iter()
            .map(|c| c.join().unwrap().expect("fixture should succeed"))
            .collect();
        assert_eq!(outputs, vec!["done: alpha", "done: beta"]);

        for (task, mut rx) in receivers {
            let mut texts = Vec::new();
            let mut finished = None;
            while let Ok(event) = rx.try_recv() {
                match event {
                    InvokeAgentProgress::Text(t) => texts.push(t),
                    InvokeAgentProgress::Finished { result, .. } => finished = result,
                    InvokeAgentProgress::Started { .. } => {}
                }
            }
            assert_eq!(
                texts,
                vec![task.to_string(), format!("✓ {task}")],
                "row for {task} sees only its own child's events"
            );
            assert_eq!(finished.as_deref(), Some(format!("done: {task}").as_str()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_progress_forwards_event_lines_and_returns_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let started = format_event_line(&SessionEvent::ToolCallStarted {
            id: "c1".into(),
            name: "read_file".into(),
        })
        .unwrap();
        let finished = format_event_line(&SessionEvent::ToolCallResult {
            id: "c1".into(),
            result: "# Chatty".into(),
        })
        .unwrap();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let script = dir.path().join("fake-tui");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 printf 'token soup' >&2\n\
                 echo >&2\n\
                 echo '{started}' >&2\n\
                 echo '  [tool: read_file] running' >&2\n\
                 echo '{finished}' >&2\n\
                 echo 'noise' >&2\n\
                 echo 'final answer'\n"
            ),
        )
        .expect("write fixture");
        let mut perms = std::fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");

        let slot = dummy_slot();
        let mut rx = install_progress_channel(&slot);
        let stdout = run_sub_agent_with_progress(
            script,
            "model-1".into(),
            "do the task".into(),
            false,
            None,
            slot,
        )
        .expect("fixture should succeed");
        assert_eq!(stdout, "final answer");

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert!(
            matches!(
                events.first(),
                Some(InvokeAgentProgress::Started {
                    agent_name,
                    prompt,
                    source: ToolSource::Local,
                }) if agent_name == "sub_agent" && prompt == "do the task"
            ),
            "expected Started first, got {events:?}"
        );
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                InvokeAgentProgress::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["read_file", "✓ read_file"]);
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("token soup") || t.contains("noise")),
            "the child's human-readable stderr must not be forwarded: {texts:?}"
        );
        assert!(
            matches!(
                events.last(),
                Some(InvokeAgentProgress::Finished {
                    success: true,
                    result: Some(result),
                }) if result == "final answer"
            ),
            "expected Finished last, got {events:?}"
        );
    }
}
