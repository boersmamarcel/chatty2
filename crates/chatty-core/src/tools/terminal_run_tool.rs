//! `terminal_run`: run one command in a terminal tab the human shared with
//! the agent as "Read + run" (AGE-584). Their tab is their own shell, outside
//! the sandbox and holding their credentials, so every command waits for
//! their approval, whatever the session's approval mode: this is the one
//! place auto-approve does not apply.
//!
//! The tool checks what it can itself (which terminal, its access, a
//! single-line command), then has the source reserve the terminal
//! ([`TerminalSource::begin_run`]: shell integration, an empty prompt, one
//! pending run per terminal) before it asks, so the human is never asked to
//! approve a command that cannot run. How the command is typed and followed
//! is the source's business ([`TerminalSource::run`]).

use std::sync::Arc;
use std::time::Duration;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::models::execution_approval_store::{PendingApprovals, request_execution_approval};
use crate::services::shell_service::{
    MAX_SHELL_CALL_TIMEOUT_SECONDS, SHELL_OUTPUT_DEFAULT_CAP_BYTES,
};
use crate::services::terminal::{
    READ_ONLY_SOURCE_MESSAGE, TerminalAccess, TerminalBackend, TerminalInfo, TerminalKind,
    TerminalSource,
};
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::ToolError;
use crate::tools::terminal_read_tool::{keep_tail, lenient_lines};

/// How long a command may run before the tool returns without it.
pub const DEFAULT_TIMEOUT_SECS: u32 = 120;

/// First line of the approval label, before the command; the desktop's
/// approval card recognises a terminal run by it ([`parse_approval_label`]).
const APPROVAL_TAG: &str = "[terminal] ";

/// The warning every terminal-run approval carries.
pub const NOT_SANDBOXED_WARNING: &str = "Runs in your shell, not the sandbox.";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct TerminalRunArgs {
    /// Terminal id (`term-2`); omitted = the read + run terminal the user
    /// was in last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// One line of shell.
    pub command: String,
    /// How long to wait for it to finish.
    #[serde(
        default,
        deserialize_with = "lenient_lines",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct TerminalRunOutput {
    pub terminal: String,
    pub title: String,
    /// The command line as the shell showed it.
    pub command: String,
    /// `None` while it is still running (and if the shell sent none).
    pub exit_code: Option<i32>,
    pub still_running: bool,
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Runs a command in the human's shared terminal, behind their approval.
#[derive(Clone)]
pub struct TerminalRunTool {
    source: Arc<dyn TerminalSource>,
    approvals: PendingApprovals,
    /// The configured `max_output_bytes`; the effective cap is the smaller
    /// of this and the shell's default cap.
    max_output_bytes: usize,
}

impl TerminalRunTool {
    pub fn new(
        source: Arc<dyn TerminalSource>,
        approvals: PendingApprovals,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            source,
            approvals,
            max_output_bytes,
        }
    }
}

/// The approval request's label: the command, where it runs, and the
/// warning. Readable as is by a frontend that shows labels verbatim.
fn approval_label(command: &str, info: &TerminalInfo) -> String {
    let cwd = info
        .cwd
        .as_deref()
        .map(|cwd| format!(" · cwd {cwd}"))
        .unwrap_or_default();
    format!(
        "{APPROVAL_TAG}{command}\nTerminal: {} ({}){cwd}\n{NOT_SANDBOXED_WARNING}",
        info.title, info.id
    )
}

/// A terminal-run approval label split into its command and the lines
/// describing where it runs; `None` for any other approval.
pub fn parse_approval_label(label: &str) -> Option<(&str, &str)> {
    let rest = label.strip_prefix(APPROVAL_TAG)?;
    Some(rest.split_once('\n').unwrap_or((rest, "")))
}

/// Why `info` takes no command, or `None` when it does.
fn refusal(info: &TerminalInfo) -> Option<String> {
    let name = format!("terminal `{}` ({})", info.id, info.title);
    if info.kind == TerminalKind::Agent {
        return Some(format!(
            "{name} is your own shell; run commands there with `shell_execute`."
        ));
    }
    if info.backend == TerminalBackend::Tmux {
        return Some(format!("{name}: {READ_ONLY_SOURCE_MESSAGE}."));
    }
    match info.access {
        TerminalAccess::ReadRun => None,
        TerminalAccess::Read => Some(format!(
            "{name} is shared with you read only, so the command was not run. Running commands \
             there needs the user to share it as \"Read + run\": tell them to click the eye icon \
             on its tab to stop sharing, click it again and choose \"Read + run\"."
        )),
        TerminalAccess::None => Some(format!(
            "{name} is not shared with you, so the command was not run. The user can share it \
             as \"Read + run\" with the eye icon on its tab."
        )),
    }
}

/// Why `command` can't be typed at a human's prompt, or `None`.
fn command_refusal(command: &str) -> Option<&'static str> {
    if command.is_empty() {
        return Some("the command is empty");
    }
    if command.contains(['\n', '\r']) {
        return Some(
            "the command has more than one line; only a single line can be run in the user's \
             terminal. Join the steps with `&&` or `;`, or run a script",
        );
    }
    if command.chars().any(char::is_control) {
        return Some(
            "the command contains control characters (a tab or an escape); type them as the \
             shell's escapes instead (`$'\\t'`)",
        );
    }
    // An odd run of trailing backslashes escapes the Enter: the shell would
    // wait at its continuation prompt and the run for its whole timeout.
    if command.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1 {
        return Some(
            "the command ends in a backslash, which would leave the user's shell waiting for \
             another line; remove it",
        );
    }
    None
}

/// Releases the terminal the source reserved, however the call ends.
struct Reserved<'a> {
    source: &'a dyn TerminalSource,
    id: &'a str,
}

impl Drop for Reserved<'_> {
    fn drop(&mut self) {
        self.source.end_run(self.id);
    }
}

impl Tool for TerminalRunTool {
    const NAME: &'static str = "terminal_run";
    type Error = ToolError;
    type Args = TerminalRunArgs;
    type Output = TerminalRunOutput;

    fn description(&self) -> String {
        format!(
            "Run one command in a terminal tab the human shared with you as \"Read + run\" \
             (`access: read_run` in `terminal_read`'s list): their own shell, e.g. their ssh \
             session, activated venv, or the dev server they are watching. It runs in the \
             human's own shell, NOT the sandbox, with their credentials. Every command needs \
             the human's approval, whatever the approval mode: auto-approve does not apply \
             here. Prefer your own shell (`shell_execute`) unless the human asked for this tab. \
             The command must be a single line. It is only typed at an empty prompt: if the \
             human is typing, or a program (vim, a REPL, a password prompt) is running there, \
             it is refused. The result has the exit code and the output. A command still \
             running after `timeout_secs` (default {DEFAULT_TIMEOUT_SECS}, a server for \
             instance) keeps running: the result says so, and you can read it later with \
             `terminal_read`. Without `terminal` it runs in the read + run terminal the human \
             used most recently."
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "terminal": {
                    "type": "string",
                    "description": "Terminal id from `terminal_read` with `list: true`, e.g. \"term-2\". Omit for the read + run terminal the user used most recently."
                },
                "command": {
                    "type": "string",
                    "description": "One line of shell to type at the prompt."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!("Seconds to wait for it to finish (default {DEFAULT_TIMEOUT_SECS}, at most {MAX_SHELL_CALL_TIMEOUT_SECONDS}).")
                }
            },
            "required": ["command"]
        })
    }

    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let fail = |message: String| ToolError::OperationFailed(message);
        let command = args.command.trim().to_string();
        if let Some(why) = command_refusal(&command) {
            return Err(fail(format!("{why}; nothing was run.")));
        }

        let terminals = self.source.list().await;
        let info = match args.terminal.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => {
                terminals.iter().find(|t| t.id == id).ok_or_else(|| {
                    fail(format!(
                        "no terminal `{id}`; `terminal_read` with `list: true` lists them."
                    ))
                })?
            }
            _ => match terminals.iter().find(|t| refusal(t).is_none()) {
                Some(info) => info,
                // A tab shared read only is the one the user most likely
                // meant: say why it can't take the command.
                None => {
                    let read_only = terminals.iter().find(|t| {
                        t.access == TerminalAccess::Read
                            && t.kind == TerminalKind::Human
                            && t.backend == TerminalBackend::Embedded
                    });
                    return Err(fail(read_only.and_then(refusal).unwrap_or_else(|| {
                        "no terminal is shared with you as \"Read + run\", so nothing was run. \
                         The user can share a terminal tab that way with the eye icon on it."
                            .to_string()
                    })));
                }
            },
        };
        if let Some(why) = refusal(info) {
            return Err(fail(why));
        }

        self.source
            .begin_run(&info.id)
            .await
            .map_err(|e| fail(format!("{e:#}; nothing was run.")))?;
        let _reserved = Reserved {
            source: self.source.as_ref(),
            id: &info.id,
        };

        // Always asked: the human's shell is outside the sandbox, so no
        // approval mode applies (AGE-584).
        let approved = request_execution_approval(
            &self.approvals,
            &ApprovalMode::AlwaysAsk,
            &approval_label(&command, info),
            false,
        )
        .await
        .map_err(|e| fail(format!("{e:#}; the command was not run.")))?;
        if !approved {
            return Err(fail(format!(
                "the user denied running `{command}` in their terminal `{}`; it was not run. \
                 Don't run it again unless they ask.",
                info.id
            )));
        }

        let timeout = args
            .timeout_secs
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_SHELL_CALL_TIMEOUT_SECONDS);
        let run = self
            .source
            .run(&info.id, &command, Duration::from_secs(timeout.into()))
            .await
            .map_err(|e| fail(format!("{e:#}")))?;

        let cap = self.max_output_bytes.min(SHELL_OUTPUT_DEFAULT_CAP_BYTES);
        let (output, dropped) = keep_tail(&run.output, cap);
        let mut notes = Vec::new();
        if !run.finished {
            notes.push(format!(
                "still running after {timeout} s; it keeps running in the user's terminal. Read \
                 it later with `terminal_read`."
            ));
        }
        if run.command != command {
            notes.push(format!(
                "the command line that ran was `{}`, not exactly what you sent.",
                run.command
            ));
        }
        if dropped > 0 {
            notes.push(format!(
                "{dropped} earlier output lines were cut to fit the tool-output budget."
            ));
        }
        Ok(TerminalRunOutput {
            terminal: info.id.clone(),
            title: info.title.clone(),
            command: run.command,
            exit_code: run.exit_code,
            still_running: !run.finished,
            output,
            truncated: dropped > 0,
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::execution_approval_store::{ApprovalDecision, ExecutionApprovalStore};
    use crate::services::terminal::{Region, TerminalRun, TerminalText};
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use tokio::sync::mpsc;

    /// Canned embedded tabs; records every call.
    struct FakeTabs {
        tabs: Vec<TerminalInfo>,
        /// What `begin_run` fails with, if anything.
        busy: Option<String>,
        /// What `run` returns.
        outcome: TerminalRun,
        calls: Mutex<Vec<String>>,
    }

    impl FakeTabs {
        fn new(tabs: &[(&str, TerminalAccess)]) -> Self {
            Self {
                tabs: tabs
                    .iter()
                    .map(|(id, access)| TerminalInfo {
                        id: id.to_string(),
                        title: format!("bash — {id}"),
                        cwd: Some("/home/me/app".into()),
                        backend: TerminalBackend::Embedded,
                        kind: TerminalKind::Human,
                        access: *access,
                    })
                    .collect(),
                busy: None,
                outcome: TerminalRun {
                    command: "make test".into(),
                    output: "ok".into(),
                    finished: true,
                    exit_code: Some(3),
                },
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().clone()
        }
    }

    #[async_trait]
    impl TerminalSource for FakeTabs {
        async fn list(&self) -> Vec<TerminalInfo> {
            self.tabs.clone()
        }
        async fn read(&self, id: &str, _: Region) -> anyhow::Result<TerminalText> {
            anyhow::bail!("not read here: {id}")
        }
        async fn begin_run(&self, id: &str) -> anyhow::Result<()> {
            self.calls.lock().push(format!("begin {id}"));
            match &self.busy {
                Some(why) => anyhow::bail!("{why}"),
                None => Ok(()),
            }
        }
        async fn run(
            &self,
            id: &str,
            command: &str,
            timeout: Duration,
        ) -> anyhow::Result<TerminalRun> {
            self.calls
                .lock()
                .push(format!("run {id} `{command}` {}s", timeout.as_secs()));
            Ok(self.outcome.clone())
        }
        fn end_run(&self, id: &str) {
            self.calls.lock().push(format!("end {id}"));
        }
    }

    /// The tool, and the approval requests it raises.
    fn tool(
        source: Arc<FakeTabs>,
    ) -> (
        TerminalRunTool,
        ExecutionApprovalStore,
        mpsc::UnboundedReceiver<crate::models::execution_approval_store::ApprovalNotification>,
    ) {
        let mut store = ExecutionApprovalStore::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let (resolved_tx, _resolved_rx) = mpsc::unbounded_channel();
        store.set_notifiers(tx, resolved_tx);
        let tool = TerminalRunTool::new(source, store.get_pending_approvals(), 51_200);
        (tool, store, rx)
    }

    async fn call(
        tool: &TerminalRunTool,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let args: TerminalRunArgs = serde_json::from_value(args).unwrap();
        // Bounded, so a call that waits on an approval nobody answers (a
        // refusal that stopped refusing) fails instead of waiting out the
        // store's own 5-minute timeout.
        let out = tokio::time::timeout(CALL_WAIT, tool.call(&mut ToolContext::new(), args))
            .await
            .expect("the call returned: it did not wait on an unanswered approval")?;
        Ok(serde_json::to_value(out).unwrap())
    }

    /// How long a test waits for an approval request, or for a call.
    const CALL_WAIT: Duration = Duration::from_secs(5);

    /// The next approval request, failing the test when none comes (the
    /// tool skipped the approval).
    async fn next_request(
        requests: &mut mpsc::UnboundedReceiver<
            crate::models::execution_approval_store::ApprovalNotification,
        >,
    ) -> crate::models::execution_approval_store::ApprovalNotification {
        tokio::time::timeout(CALL_WAIT, requests.recv())
            .await
            .expect("an approval request: the tool must always ask")
            .expect("the approval channel is open")
    }

    /// Call the tool while answering its approval request with `approve`;
    /// returns the result and the approval label.
    async fn call_answering(
        source: Arc<FakeTabs>,
        args: serde_json::Value,
        approve: bool,
    ) -> (Result<serde_json::Value, ToolError>, String) {
        let (tool, store, mut requests) = tool(source);
        let answer = tokio::spawn(async move {
            let request = next_request(&mut requests).await;
            let decision = if approve {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::Denied
            };
            assert!(store.resolve(&request.id, decision));
            assert!(!request.is_sandboxed);
            request.command
        });
        let result = call(&tool, args).await;
        (result, answer.await.unwrap())
    }

    /// Access levels: only a tab shared as read + run takes a command; the
    /// others are refused before anything is reserved or asked, a read-only
    /// one with a pointer at the share setting.
    #[tokio::test]
    async fn only_a_read_run_tab_takes_a_command() {
        let source = Arc::new(FakeTabs::new(&[
            ("term-1", TerminalAccess::None),
            ("term-2", TerminalAccess::Read),
            ("term-3", TerminalAccess::ReadRun),
        ]));
        let (tool, _store, mut requests) = tool(source.clone());

        let err = call(
            &tool,
            serde_json::json!({"terminal": "term-1", "command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("not shared with you"), "{err}");
        assert!(err.contains("Read + run"), "{err}");

        let err = call(
            &tool,
            serde_json::json!({"terminal": "term-2", "command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("read only"), "{err}");
        assert!(err.contains("eye icon"), "{err}");
        assert!(source.calls().is_empty(), "{:?}", source.calls());
        assert!(requests.try_recv().is_err(), "no approval was asked");

        // Omitted, the read + run tab is the target.
        let (out, label) = call_answering(
            source.clone(),
            serde_json::json!({"command": "make test"}),
            true,
        )
        .await;
        assert_eq!(out.unwrap()["terminal"], "term-3");
        assert!(label.contains("make test"), "{label}");
        assert_eq!(
            source.calls(),
            ["begin term-3", "run term-3 `make test` 120s", "end term-3"]
        );
    }

    /// Nothing shared as read + run: said so; a tmux pane and the agent's
    /// own tab are never run in.
    #[tokio::test]
    async fn no_read_run_tab_tmux_and_the_agent_tab_are_refused() {
        let mut tabs = FakeTabs::new(&[("term-1", TerminalAccess::Read)]);
        let err = call(
            &tool(Arc::new(FakeTabs::new(&[]))).0,
            serde_json::json!({"command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("no terminal is shared with you as"), "{err}");
        // Not named, and only a read-only tab: that tab's refusal, pointing
        // at the share setting.
        let err = call(
            &tool(Arc::new(FakeTabs::new(&[("term-1", TerminalAccess::Read)]))).0,
            serde_json::json!({"command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("`term-1`"), "{err}");
        assert!(err.contains("read only"), "{err}");

        tabs.tabs.push(TerminalInfo {
            id: "%3".into(),
            title: "vim".into(),
            cwd: None,
            backend: TerminalBackend::Tmux,
            kind: TerminalKind::Human,
            access: TerminalAccess::ReadRun,
        });
        tabs.tabs.push(TerminalInfo {
            id: "term-9".into(),
            title: "Agent".into(),
            cwd: None,
            backend: TerminalBackend::Embedded,
            kind: TerminalKind::Agent,
            access: TerminalAccess::ReadRun,
        });
        let (tool, _store, _requests) = tool(Arc::new(tabs));
        let err = call(
            &tool,
            serde_json::json!({"terminal": "%3", "command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("tmux panes are read only"), "{err}");
        let err = call(
            &tool,
            serde_json::json!({"terminal": "term-9", "command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("shell_execute"), "{err}");
    }

    /// No integration, a busy prompt, a pending run: the source's refusal
    /// comes back before the human is asked.
    #[tokio::test]
    async fn a_terminal_that_cannot_run_is_refused_before_asking() {
        let mut tabs = FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]);
        tabs.busy = Some(
            "this terminal has no shell integration; I can't tell when a command finishes".into(),
        );
        let source = Arc::new(tabs);
        let (tool, _store, mut requests) = tool(source.clone());
        let err = call(
            &tool,
            serde_json::json!({"terminal": "term-3", "command": "ls"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("no shell integration"), "{err}");
        assert!(err.contains("nothing was run"), "{err}");
        assert!(requests.try_recv().is_err(), "no approval was asked");
        assert_eq!(source.calls(), ["begin term-3"]);
    }

    /// Multi-line and control characters are refused outright.
    #[tokio::test]
    async fn only_single_line_commands_are_typed() {
        let source = Arc::new(FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]));
        let (tool, _store, _requests) = tool(source.clone());
        for command in [
            "echo a\necho b",
            "printf 'a\tb'",
            "   ",
            "ls \\",
            "echo a \\\\\\",
        ] {
            let err = call(
                &tool,
                serde_json::json!({"terminal": "term-3", "command": command}),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(err.contains("nothing was run"), "{command:?}: {err}");
        }
        assert!(source.calls().is_empty());
        // An escaped backslash at the end is a complete line.
        assert_eq!(command_refusal("echo a\\\\"), None);
        assert!(command_refusal("ls \\").unwrap().contains("backslash"));
    }

    /// Denied: a refusal, nothing typed, the terminal released.
    #[tokio::test]
    async fn a_denied_command_is_not_run() {
        let source = Arc::new(FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]));
        let (result, label) = call_answering(
            source.clone(),
            serde_json::json!({"terminal": "term-3", "command": "rm -rf build"}),
            false,
        )
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("not run"), "{err}");
        assert_eq!(source.calls(), ["begin term-3", "end term-3"]);
        // The card shows the command, the tab and its directory, and the
        // warning.
        let (command, context) = parse_approval_label(&label).expect("a terminal-run label");
        assert_eq!(command, "rm -rf build");
        assert!(context.contains("bash — term-3 (term-3)"), "{context}");
        assert!(context.contains("cwd /home/me/app"), "{context}");
        assert!(context.contains(NOT_SANDBOXED_WARNING), "{context}");
        assert_eq!(parse_approval_label("[git] commit"), None);
    }

    /// The tool is built without the session's approval mode at all: even
    /// where the session auto-approves everything (the factory builds it
    /// the same way under `AutoApproveAll`), nothing runs until the human
    /// answers the card.
    #[tokio::test]
    async fn approval_is_asked_even_when_everything_else_is_auto_approved() {
        let source = Arc::new(FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]));
        let (tool, store, mut requests) = tool(source.clone());
        let pending = tokio::spawn(async move {
            call(
                &tool,
                serde_json::json!({"terminal": "term-3", "command": "make test"}),
            )
            .await
        });
        let request = next_request(&mut requests).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let calls = source.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("run ")),
            "not run before the answer: {calls:?}"
        );
        assert_eq!(calls, ["begin term-3"]);
        assert!(store.resolve(&request.id, ApprovalDecision::Approved));
        let out = pending.await.unwrap().unwrap();
        assert_eq!(out["exit_code"], 3);
    }

    /// Exit code round trip and the timeout path.
    #[tokio::test]
    async fn exit_code_and_still_running_reach_the_model() {
        let source = Arc::new(FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]));
        let (out, _) = call_answering(
            source.clone(),
            serde_json::json!({"terminal": "term-3", "command": "make test"}),
            true,
        )
        .await;
        let out = out.unwrap();
        assert_eq!(out["exit_code"], 3);
        assert_eq!(out["still_running"], false);
        assert_eq!(out["output"], "ok");
        assert!(out.get("note").is_none(), "{out}");

        let mut tabs = FakeTabs::new(&[("term-3", TerminalAccess::ReadRun)]);
        tabs.outcome = TerminalRun {
            command: "python3 -m http.server 8765".into(),
            output: "Serving HTTP on 0.0.0.0 port 8765".into(),
            finished: false,
            exit_code: None,
        };
        let source = Arc::new(tabs);
        let (out, _) = call_answering(
            source.clone(),
            serde_json::json!({
                "terminal": "term-3",
                "command": "python3 -m http.server 8765",
                "timeout_secs": "5"
            }),
            true,
        )
        .await;
        let out = out.unwrap();
        assert_eq!(out["still_running"], true);
        assert!(out["exit_code"].is_null());
        let note = out["note"].as_str().unwrap();
        assert!(note.contains("still running after 5 s"), "{note}");
        assert!(note.contains("terminal_read"), "{note}");
        assert!(source.calls()[1].ends_with(" 5s"), "{:?}", source.calls());
    }
}
