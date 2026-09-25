//! Agent loop guard — shared across frontends (TUI and GPUI).
//!
//! Detects two classes of runaway agent behavior and returns corrective
//! injection prompts that the frontend can send as the next user message:
//!
//! 1. **Repeated tool calls** — the model calls the same tool with identical
//!    arguments two or more times in a row. A pivot prompt is returned so the
//!    agent tries a different approach instead of spinning.
//!
//! 2. **Late-game stall** — when the agent is within `LATE_GAME_THRESHOLD`
//!    turns of the per-conversation limit and still has not produced an answer
//!    file, a deadline prompt is returned once to force a commit.
//!
//! 3. **Verbosity guard** — tracks how many bytes of plain text have been
//!    emitted in the current turn without a tool call. Returns `true` when the
//!    soft limit is exceeded so the frontend can stop the stream early.
//!
//! 4. **Busy without progress** (unattended runs only, opt-in through
//!    [`AgentLoopGuard::with_progress_check`]) — [`PROGRESS_WINDOW_TOOL_CALLS`]
//!    tool calls in a row that write no file, run no new test command and
//!    read no file not read before earn one nudge to change approach or
//!    finish; a second such window asks the frontend to finalize. SWE-bench
//!    runs of a local model went 310–450 tool calls without either. Only a
//!    write inside the working tree counts: a scratch script under `/tmp`,
//!    written and rewritten again and again, is not progress on the task.
//!
//! ## Usage (in both frontends)
//!
//! ```ignore
//! let mut guard = AgentLoopGuard::new(max_agent_turns, answer_file_required);
//!
//! // After each tool result:
//! if let Some(pivot) = guard.on_tool_completed(&name, &input) {
//!     engine.send_message(pivot);
//! }
//!
//! // At the end of each stream turn:
//! guard.on_turn_complete(turns_used, has_answer);
//! if let Some(deadline) = guard.deadline_message() {
//!     engine.send_message(deadline.to_string());
//! }
//!
//! // During TextChunk handling:
//! if guard.on_text_chunk(chunk.len()) {
//!     engine.stop_stream(); // verbosity limit exceeded
//! }
//! ```

use std::collections::{HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};

// ── Tunables ──────────────────────────────────────────────────────────────────

/// How many bytes of pure text (no tool call) in one turn triggers the soft
/// verbosity stop.  Kept lower than the TUI hard-stop constant so frontends
/// get an early warning rather than waiting for an 8 KB runaway.
const VERBOSITY_SOFT_LIMIT_BYTES: usize = 4_000;

/// Maximum number of loop-pivot injections per agent session.  After this the
/// guard stops firing so we don't loop on the pivot itself.
const MAX_LOOP_PIVOTS: usize = 3;

/// How many turns before the hard cap to inject the deadline prompt.
const LATE_GAME_THRESHOLD: usize = 2;

/// Ring-buffer size for recent tool calls used for repetition detection.
const TOOL_CALL_HISTORY_LEN: usize = 6;

/// Tool calls in a row without progress (no file written, no new test
/// command, no file read for the first time) before the progress check
/// nudges, and again before it asks for finalization. On the SWE-bench runs
/// that stalled, Codex CLI solved whole tasks in ~66 calls; 25 calls of pure
/// re-reading and searching is a third of that.
pub const PROGRESS_WINDOW_TOOL_CALLS: usize = 25;

/// Tools that write a file.
const FILE_WRITE_TOOLS: &[&str] = &[
    "write_file",
    "apply_diff",
    "delete_file",
    "move_file",
    "create_directory",
    "final_answer",
    "write_docx",
    "write_excel",
    "edit_excel",
    "write_pptx",
];

/// Tools that run a command.
const COMMAND_TOOLS: &[&str] = &["shell_execute", "execute_code"];

/// Tools whose path argument names what they write; the rest of
/// [`FILE_WRITE_TOOLS`] (answer files, documents) always count.
const PATH_WRITE_TOOLS: &[(&str, &str)] = &[
    ("write_file", "path"),
    ("apply_diff", "path"),
    ("delete_file", "path"),
    ("create_directory", "path"),
    ("move_file", "destination"),
];

/// Scratch directories when the run has no workspace root to compare with.
const SCRATCH_DIRS: &[&str] = &["/tmp", "/var/tmp", "/dev"];

/// A command containing one of these runs tests.
const TEST_RUNNER_MARKERS: &[&str] = &[
    "pytest",
    "unittest",
    "tox",
    "nox",
    "runtests",
    "cargo test",
    "go test",
    "npm test",
    "npm run test",
    "yarn test",
    "pnpm test",
    "jest",
    "vitest",
    "mocha",
    "make test",
    "make check",
    "mvn test",
    "gradle test",
    "ctest",
    "rspec",
    "phpunit",
];

/// Research tools, with the argument that names what they look at: a URL
/// fetched, a query searched, a code snippet run. The first call on a target
/// is progress: a GAIA run reading one new page after another is working,
/// not spinning.
const RESEARCH_TOOLS: &[(&str, &str)] = &[
    ("fetch", "url"),
    ("browser_navigate", "url"),
    ("browser_use", "task"),
    ("search_web", "query"),
    ("doc_retriever", "query"),
    ("query_data", "query"),
    ("execute_code", "code"),
];

/// Shell commands whose file arguments are a read.
const SHELL_READERS: &[&str] = &["cat", "head", "tail", "sed", "nl", "less", "more", "bat"];

/// The progress check's verdict on a finished tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressCheck {
    /// Progress, or still inside the window.
    Fine,
    /// A window without progress: send this once, then carry on.
    Nudge(String),
    /// A second window without progress after the nudge: finalize.
    Finalize,
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Stateful guard that detects and corrects runaway agent loops.
///
/// Construct one per agent invocation (per message send) and thread it through
/// the event loop.  Both `chatty-tui` (headless mode) and `chatty-gpui`
/// (desktop app) should use this so the behaviour is consistent.
pub struct AgentLoopGuard {
    /// Whether the benchmark task expects an answer file to be produced.
    answer_file_required: bool,

    /// Hard maximum number of agent turns for this session.
    max_agent_turns: usize,

    /// Ring buffer of `(tool_name, trimmed_input)` for the last N tool calls.
    /// The full (untruncated) input is kept here so repetition detection isn't
    /// fooled by two different calls sharing a long common prefix; truncation
    /// is applied only when building the pivot message preview.
    recent_tool_calls: VecDeque<(String, String)>,

    /// How many loop-pivot prompts have been injected so far.
    loop_pivot_count: usize,

    /// Whether the late-game deadline prompt has already been injected.
    late_game_injected: bool,

    /// Bytes of plain text emitted in the current turn (reset on `on_turn_complete`).
    text_bytes_this_turn: usize,

    /// Whether a tool was called in the current turn (resets verbosity counter).
    tool_called_this_turn: bool,

    /// Pending deadline message to be retrieved after `on_turn_complete`.
    pending_deadline: Option<String>,

    /// Whether the busy-without-progress check runs (unattended runs only).
    progress_check: bool,

    /// Tool calls since the last one that made progress.
    calls_without_progress: usize,

    /// Whether the current stretch without progress was already nudged.
    progress_nudged: bool,

    /// Files read so far, and test commands run so far.
    seen_files: HashSet<String>,
    seen_test_commands: HashSet<String>,
    /// URLs fetched, queries searched and code run so far, as `tool:target`.
    seen_targets: HashSet<String>,
    /// The working tree: a write outside it (a `/tmp` scratch script) is
    /// not progress. Without one, only the scratch dirs count as outside.
    workspace_root: Option<PathBuf>,
}

impl AgentLoopGuard {
    /// Create a new guard for a single agent invocation.
    ///
    /// - `max_agent_turns`: the `execution_settings.max_agent_turns` value.
    /// - `answer_file_required`: true when the task prompt mentions `answer.txt`.
    pub fn new(max_agent_turns: usize, answer_file_required: bool) -> Self {
        Self {
            answer_file_required,
            max_agent_turns,
            recent_tool_calls: VecDeque::with_capacity(TOOL_CALL_HISTORY_LEN),
            loop_pivot_count: 0,
            late_game_injected: false,
            text_bytes_this_turn: 0,
            tool_called_this_turn: false,
            pending_deadline: None,
            progress_check: false,
            calls_without_progress: 0,
            progress_nudged: false,
            seen_files: HashSet::new(),
            seen_test_commands: HashSet::new(),
            seen_targets: HashSet::new(),
            workspace_root: None,
        }
    }

    /// Turn on the busy-without-progress check. Only unattended runs do:
    /// someone watching an interactive chat is its progress check.
    pub fn with_progress_check(mut self) -> Self {
        self.progress_check = true;
        self
    }

    /// The run's working tree, for the progress check: writes outside it do
    /// not count as progress. Relative paths are taken to be inside it.
    pub fn with_workspace_root(mut self, root: Option<&Path>) -> Self {
        self.workspace_root = root.map(|root| {
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            normalize(&root)
        });
        self
    }

    /// Call after each completed tool result (success or error), with the
    /// call's JSON arguments. Returns what the progress check makes of the
    /// run so far; always [`ProgressCheck::Fine`] when the check is off.
    pub fn on_tool_progress(&mut self, name: &str, input: &str) -> ProgressCheck {
        if !self.progress_check {
            return ProgressCheck::Fine;
        }
        if self.made_progress(name, input) {
            self.calls_without_progress = 0;
            self.progress_nudged = false;
            return ProgressCheck::Fine;
        }
        self.calls_without_progress += 1;
        if self.calls_without_progress < PROGRESS_WINDOW_TOOL_CALLS {
            return ProgressCheck::Fine;
        }
        self.calls_without_progress = 0;
        if self.progress_nudged {
            return ProgressCheck::Finalize;
        }
        self.progress_nudged = true;
        ProgressCheck::Nudge(format!(
            "PROGRESS CHECK: your last {PROGRESS_WINDOW_TOOL_CALLS} tool calls wrote no file, ran \
             no new test and read no new file. Stop re-reading and searching. Either change \
             approach now — make the edit you have evidence for and run the most specific test — \
             or, if you cannot get further, finish with your best result so far."
        ))
    }

    /// Whether this call moved the run forward: it wrote a file inside the
    /// working tree, ran a test command not run before, read a file not read
    /// before, or fetched, searched or ran something new ([`RESEARCH_TOOLS`]).
    ///
    /// A write outside the working tree — a scratch script under `/tmp`, or
    /// the tenth rewrite of one — is not progress: a run can write a new
    /// debug script every call without getting any closer to the change.
    fn made_progress(&mut self, name: &str, input: &str) -> bool {
        let args: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
        if FILE_WRITE_TOOLS.contains(&name) {
            let target = PATH_WRITE_TOOLS
                .iter()
                .find(|(tool, _)| *tool == name)
                .and_then(|(_, key)| args.get(*key)?.as_str());
            return match target {
                Some(path) => self.note_write(path),
                None => true,
            };
        }
        if let Some(target) = research_target(name, &args)
            && self.seen_targets.insert(format!("{name}:{target}"))
        {
            return true;
        }
        if COMMAND_TOOLS.contains(&name) {
            let command = args
                .get("command")
                .or_else(|| args.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or(input);
            let writes = shell_writes(command);
            let mut wrote_in_tree = writes.untargeted;
            for target in &writes.targets {
                wrote_in_tree |= self.note_write(target);
            }
            if wrote_in_tree {
                return true;
            }
            if TEST_RUNNER_MARKERS.iter().any(|m| command.contains(m))
                && self.seen_test_commands.insert(command.trim().to_string())
            {
                return true;
            }
            let mut new_read = false;
            for path in shell_read_paths(command) {
                new_read |= self.seen_files.insert(path);
            }
            return new_read;
        }
        if (name.starts_with("read_") || name.starts_with("pdf_"))
            && let Some(path) = args.get("path").and_then(|v| v.as_str())
        {
            return self.seen_files.insert(path.to_string());
        }
        false
    }

    /// A write to `path`: whether it is inside the working tree. A scratch
    /// file outside it is remembered as read, so reading it back is no new
    /// read either.
    fn note_write(&mut self, path: &str) -> bool {
        if self.in_tree(path) {
            return true;
        }
        self.seen_files.insert(path.to_string());
        false
    }

    /// Whether `path` (as the model wrote it) is inside the working tree.
    fn in_tree(&self, path: &str) -> bool {
        let path = path.trim_matches(|c| c == '\'' || c == '"');
        if path.is_empty() {
            return true;
        }
        if path.starts_with('~') || path.starts_with('$') {
            return false;
        }
        let path = Path::new(path);
        if !path.is_absolute() {
            return true;
        }
        let path = normalize(path);
        match &self.workspace_root {
            Some(root) => path.starts_with(root),
            None => {
                let temp = normalize(&std::env::temp_dir());
                !(SCRATCH_DIRS.iter().any(|dir| path.starts_with(dir))
                    || (temp.parent().is_some() && path.starts_with(&temp)))
            }
        }
    }

    // ── Event handlers ────────────────────────────────────────────────────────

    /// Call after each completed tool result (success or error).
    ///
    /// Returns a pivot message to inject if the same `(name, input)` pair has
    /// appeared at least twice consecutively in the recent history.
    pub fn on_tool_completed(&mut self, name: &str, input: &str) -> Option<String> {
        let entry = (name.to_string(), input.trim().to_string());
        self.tool_called_this_turn = true;

        // Add to ring buffer.
        if self.recent_tool_calls.len() >= TOOL_CALL_HISTORY_LEN {
            self.recent_tool_calls.pop_front();
        }
        self.recent_tool_calls.push_back(entry);

        // Check if the last two entries are identical.
        if self.loop_pivot_count >= MAX_LOOP_PIVOTS {
            return None;
        }
        let len = self.recent_tool_calls.len();
        if len < 2 {
            return None;
        }
        let last = &self.recent_tool_calls[len - 1];
        let prev = &self.recent_tool_calls[len - 2];
        if last == prev {
            self.loop_pivot_count += 1;
            let input_preview = truncate_input(&last.1);
            Some(format!(
                "LOOP DETECTED: You just called `{name}` with the same arguments twice in a row \
                 (input: {input_preview}). That approach is not working. \
                 Switch to a completely different strategy — try a different tool, \
                 different search terms, a different API, or different computation method. \
                 Do NOT repeat the same call again."
            ))
        } else {
            None
        }
    }

    /// Call at the end of each stream turn (on `StreamCompleted`).
    ///
    /// - `turns_used`: number of assistant turns completed so far.
    /// - `has_answer`: whether the answer file already exists.
    ///
    /// After calling this, check [`Self::deadline_message`] to see if a
    /// deadline prompt should be injected.
    pub fn on_turn_complete(&mut self, turns_used: usize, has_answer: bool) {
        // Reset per-turn verbosity tracking.
        self.text_bytes_this_turn = 0;
        self.tool_called_this_turn = false;

        // Fire deadline at most once, only when answer is still missing.
        if self.late_game_injected
            || !self.answer_file_required
            || has_answer
            || self.max_agent_turns == 0
        {
            self.pending_deadline = None;
            return;
        }

        let remaining = self.max_agent_turns.saturating_sub(turns_used);
        if remaining <= LATE_GAME_THRESHOLD {
            self.late_game_injected = true;
            self.pending_deadline = Some(format!(
                "DEADLINE: Only {remaining} turn(s) remaining and required output is still missing. \
                 Provide your best final answer now instead of continuing to research. \
                 Use the available response tool/output channel immediately."
            ));
        } else {
            self.pending_deadline = None;
        }
    }

    /// Returns the pending deadline message set by the last `on_turn_complete`
    /// call, if any.  Consuming the message: caller should drain this after
    /// checking so it doesn't re-fire on the next call.
    pub fn take_deadline_message(&mut self) -> Option<String> {
        self.pending_deadline.take()
    }

    /// Call for each text chunk received from the LLM stream (before any tool
    /// call in the current turn).
    ///
    /// Returns `true` when the soft verbosity limit is exceeded — the frontend
    /// should inject a recovery prompt once the stream finishes. Callers that
    /// only want to react once per turn should de-duplicate the `true` result.
    pub fn on_text_chunk(&mut self, bytes: usize) -> bool {
        if self.tool_called_this_turn {
            return false;
        }
        self.text_bytes_this_turn += bytes;
        self.text_bytes_this_turn > VERBOSITY_SOFT_LIMIT_BYTES
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Number of loop pivots injected so far (useful for logging).
    pub fn loop_pivot_count(&self) -> usize {
        self.loop_pivot_count
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// What a research tool call looks at, normalised so the same page or query
/// is recognised again; `None` for any other tool. A `fetch` further into a
/// page (`start_index`) is a new target: it reads what was not read yet.
fn research_target(name: &str, args: &serde_json::Value) -> Option<String> {
    let (_, key) = RESEARCH_TOOLS.iter().find(|(tool, _)| *tool == name)?;
    let value = args.get(*key)?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    let mut target = if *key == "code" {
        value.to_string()
    } else {
        value.to_lowercase()
    };
    if let Some(start) = args.get("start_index").and_then(|v| v.as_u64()) {
        target.push_str(&format!("@{start}"));
    }
    Some(target)
}

/// `path` with `.` and `..` resolved lexically (nothing is looked up).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// What a shell command (or a code snippet) writes.
#[derive(Debug, Default, PartialEq, Eq)]
struct ShellWrites {
    /// The paths written: redirection targets, `tee`/`sed -i`/`cp`/`mv`
    /// destinations, files a Python `open(..., "w")` names.
    targets: Vec<String>,
    /// A write whose target cannot be read off the command (`git apply`,
    /// `patch`, `open(path_var, "w")`): counted as a write in the tree.
    untargeted: bool,
}

/// Commands that feed a here-document to a file: its body is file content,
/// not commands, so it is not searched for writes.
const HEREDOC_DATA_COMMANDS: &[&str] = &["cat", "tee"];

/// The writes in a shell command line. Quoted text is not taken for shell
/// syntax (`python -c "if a > b: ..."` redirects nothing); it and the body
/// of a here-document fed to an interpreter are searched for Python file
/// writes instead.
fn shell_writes(command: &str) -> ShellWrites {
    let mut writes = ShellWrites::default();
    let (shell, code) = split_heredocs(command);
    for segment in shell_segments(&shell) {
        segment_writes(&segment, &mut writes);
    }
    python_writes(&shell, &mut writes);
    python_writes(&code, &mut writes);
    writes
}

/// `command` without its here-document bodies, and the bodies that are
/// code (fed to an interpreter rather than written to a file by `cat`).
fn split_heredocs(command: &str) -> (String, String) {
    let mut shell = String::new();
    let mut code = String::new();
    let mut lines = command.lines();
    while let Some(line) = lines.next() {
        shell.push_str(line);
        shell.push('\n');
        let Some(at) = line.find("<<") else {
            continue;
        };
        let rest = line[at + 2..].trim_start_matches(['-', '~']).trim_start();
        let delimiter: String = rest
            .trim_start_matches(['\'', '"'])
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if delimiter.is_empty() {
            continue; // `<<<` here-string, or not a here-document at all
        }
        let is_data = line
            .split(['|', ';', '&'])
            .find(|part| part.contains("<<"))
            .and_then(|part| part.split_whitespace().next())
            .is_some_and(|program| HEREDOC_DATA_COMMANDS.contains(&program));
        for body in lines.by_ref() {
            if body.trim() == delimiter {
                break;
            }
            if !is_data {
                code.push_str(body);
                code.push('\n');
            }
        }
    }
    (shell, code)
}

/// A word of a shell command line, or a redirection of output to a file.
#[derive(Debug, PartialEq, Eq)]
enum ShellToken {
    Word(String),
    /// `>`, `>>`, `2>`, `&>`: the next word is the file written.
    RedirectOut,
    /// `<`, `<<`, `<<<`: the next word is read (or a here-document's
    /// delimiter), not an argument.
    RedirectIn,
}

/// The simple commands of a shell command line (split on unquoted `|`,
/// `;`, `&` and newlines), as tokens. Quotes are removed from words; an
/// output redirection to another descriptor (`2>&1`) is dropped.
fn shell_segments(command: &str) -> Vec<Vec<ShellToken>> {
    let mut segments = Vec::new();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let flush = |word: &mut String, tokens: &mut Vec<ShellToken>| {
        if !word.is_empty() {
            tokens.push(ShellToken::Word(std::mem::take(word)));
        }
    };
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            '>' => {
                // A descriptor number or `&` right before `>` belongs to it.
                if word.chars().all(|c| c.is_ascii_digit()) || word == "&" {
                    word.clear();
                } else {
                    flush(&mut word, &mut tokens);
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `>&2`: to another descriptor, not a file.
                    chars.next();
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        chars.next();
                    }
                } else {
                    tokens.push(ShellToken::RedirectOut);
                }
            }
            '<' => {
                flush(&mut word, &mut tokens);
                while chars
                    .peek()
                    .is_some_and(|c| *c == '<' || *c == '-' || *c == '~')
                {
                    chars.next();
                }
                tokens.push(ShellToken::RedirectIn);
            }
            '&' if chars.peek() == Some(&'>') => {
                flush(&mut word, &mut tokens);
                word.push('&');
            }
            '|' | ';' | '&' | '\n' => {
                flush(&mut word, &mut tokens);
                if !tokens.is_empty() {
                    segments.push(std::mem::take(&mut tokens));
                }
            }
            c if c.is_whitespace() => flush(&mut word, &mut tokens),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut tokens);
    if !tokens.is_empty() {
        segments.push(tokens);
    }
    segments
}

/// The writes of one simple command: its output redirections, and what
/// `tee`, `sed -i`, `cp`, `mv`, `git apply` and `patch` write.
fn segment_writes(tokens: &[ShellToken], writes: &mut ShellWrites) {
    let mut words = Vec::new();
    let mut pending: Option<&ShellToken> = None;
    for token in tokens {
        match (token, pending.take()) {
            (ShellToken::Word(word), Some(ShellToken::RedirectOut)) => {
                writes.targets.push(word.clone());
            }
            (ShellToken::Word(_), Some(_)) => {}
            (ShellToken::Word(word), None) => words.push(word.as_str()),
            (redirection, _) => pending = Some(redirection),
        }
    }
    let Some((&program, args)) = words.split_first() else {
        return;
    };
    let operands = || args.iter().copied().filter(|a| !a.starts_with('-'));
    match program {
        "tee" => writes.targets.extend(operands().map(str::to_string)),
        "sed"
            if args
                .iter()
                .any(|a| a.starts_with("-i") || *a == "--in-place") =>
        {
            // The files are the operands after the script: the first
            // operand, unless `-e`/`-f` gave the script.
            let mut files = Vec::new();
            let mut script_given = false;
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                if matches!(*arg, "-e" | "-f" | "--expression" | "--file") {
                    script_given = true;
                    args.next();
                } else if !arg.starts_with('-') {
                    files.push(arg.to_string());
                }
            }
            if !script_given && !files.is_empty() {
                files.remove(0);
            }
            writes.targets.extend(files);
        }
        "cp" | "mv" | "install" => {
            if let Some(destination) = operands().next_back()
                && operands().count() >= 2
            {
                writes.targets.push(destination.to_string());
            }
        }
        "patch" => writes.untargeted = true,
        "git" if args.first() == Some(&"apply") => writes.untargeted = true,
        _ => {}
    }
}

/// Python file writes in `code`: `open(<path>, "w" | "a" | "x" ...)` and
/// `Path(<path>).write_text(...)` / `.write_bytes(...)`. A write whose path
/// is not a string literal marks the command `untargeted`.
fn python_writes(code: &str, writes: &mut ShellWrites) {
    let mut rest = code;
    while let Some(at) = rest.find("open(") {
        let preceded_by_ident = rest[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        rest = &rest[at + "open(".len()..];
        if preceded_by_ident {
            continue; // `os.popen(`, `urlopen(`
        }
        let call = &rest[..rest.find(')').unwrap_or(rest.len())];
        let (path, after_path) = match string_literal(call) {
            Some((path, after)) => (Some(path), after),
            None => (None, call.split_once(',').map_or("", |(_, after)| after)),
        };
        let writes_file = string_literals(after_path).iter().any(|mode| {
            !mode.is_empty()
                && mode.chars().all(|c| "rwxabt+".contains(c))
                && mode.chars().any(|c| "wax".contains(c))
        });
        if writes_file {
            match path {
                Some(path) => writes.targets.push(path),
                None => writes.untargeted = true,
            }
        }
    }
    for method in [".write_text(", ".write_bytes("] {
        let mut rest = code;
        while let Some(at) = rest.find(method) {
            let before = rest[..at].trim_end();
            rest = &rest[at + method.len()..];
            let path = before
                .strip_suffix(')')
                .and_then(|b| b.rfind("Path(").map(|p| &b[p + "Path(".len()..]))
                .and_then(|inner| string_literal(inner))
                .filter(|(_, after)| after.trim().is_empty())
                .map(|(path, _)| path);
            match path {
                Some(path) => writes.targets.push(path),
                None => writes.untargeted = true,
            }
        }
    }
}

/// The string literal `text` starts with (after whitespace and a `r`/`b`
/// prefix), and the text after it.
fn string_literal(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start().trim_start_matches(['r', 'b']);
    let quote = text.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let body = &text[1..];
    let end = body.find(quote)?;
    Some((body[..end].to_string(), &body[end + 1..]))
}

/// Every string literal in `text`.
fn string_literals(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['\'', '"']) {
        match string_literal(&rest[at..]) {
            Some((literal, after)) => {
                found.push(literal);
                rest = after;
            }
            None => break,
        }
    }
    found
}

/// The file arguments of the reading commands (`cat`, `sed -n`, `head` …)
/// in a shell command line, split on the usual separators.
fn shell_read_paths(command: &str) -> Vec<String> {
    command
        .split(['|', ';', '&', '\n'])
        .filter_map(|segment| {
            let mut words = segment.split_whitespace();
            let program = words.next()?;
            SHELL_READERS
                .contains(&program)
                .then(|| words.filter(|w| looks_like_path(w)).map(str::to_string))
        })
        .flatten()
        .collect()
}

fn looks_like_path(word: &str) -> bool {
    !word.starts_with('-')
        && !word.starts_with('\'')
        && !word.starts_with('"')
        && (word.contains('/') || word.contains('.'))
        && !word
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == 'p' || c == '.')
}

/// Truncate and clean a tool input string for use in loop detection and
/// in pivot prompt messages.  We only need enough to distinguish two calls.
fn truncate_input(input: &str) -> String {
    let clean = input.trim();
    if clean.chars().count() <= 120 {
        clean.to_string()
    } else {
        let head: String = clean.chars().take(120).collect();
        format!("{head}…")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> AgentLoopGuard {
        AgentLoopGuard::new(20, true)
    }

    #[test]
    fn no_pivot_on_first_call() {
        let mut g = guard();
        assert!(g.on_tool_completed("search_web", "rust async").is_none());
    }

    #[test]
    fn no_pivot_when_calls_differ() {
        let mut g = guard();
        g.on_tool_completed("search_web", "rust async");
        assert!(
            g.on_tool_completed("search_web", "different query")
                .is_none()
        );
    }

    #[test]
    fn pivot_on_identical_consecutive_calls() {
        let mut g = guard();
        g.on_tool_completed("search_web", "rust async");
        let result = g.on_tool_completed("search_web", "rust async");
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.contains("search_web"));
        assert!(msg.contains("LOOP DETECTED"));
    }

    #[test]
    fn no_pivot_when_calls_share_only_a_long_common_prefix() {
        let mut g = guard();
        let shared_prefix = "a".repeat(120);
        let first = format!("{shared_prefix} version one");
        let second = format!("{shared_prefix} version two");
        g.on_tool_completed("write_file", &first);
        assert!(g.on_tool_completed("write_file", &second).is_none());
    }

    #[test]
    fn pivot_capped_at_max() {
        let mut g = guard();
        for _ in 0..MAX_LOOP_PIVOTS {
            g.on_tool_completed("search_web", "q");
            g.on_tool_completed("search_web", "q");
        }
        // After MAX_LOOP_PIVOTS the guard returns None.
        let result = g.on_tool_completed("search_web", "q");
        assert!(result.is_none());
    }

    #[test]
    fn no_deadline_with_turns_remaining() {
        let mut g = guard();
        g.on_turn_complete(5, false);
        assert!(g.take_deadline_message().is_none());
    }

    #[test]
    fn deadline_fires_near_end() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, false); // 2 remaining
        assert!(g.take_deadline_message().is_some());
    }

    #[test]
    fn deadline_not_fired_when_answer_exists() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, true); // has_answer = true
        assert!(g.take_deadline_message().is_none());
    }

    #[test]
    fn deadline_fires_only_once() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, false);
        let first = g.take_deadline_message();
        g.on_turn_complete(9, false);
        let second = g.take_deadline_message();
        assert!(first.is_some());
        assert!(second.is_none()); // already fired
    }

    #[test]
    fn verbosity_ok_below_limit() {
        let mut g = guard();
        assert!(!g.on_text_chunk(100));
        assert!(!g.on_text_chunk(100));
    }

    #[test]
    fn verbosity_triggers_above_limit() {
        let mut g = guard();
        // Feed bytes over the limit
        let over = VERBOSITY_SOFT_LIMIT_BYTES + 1;
        assert!(g.on_text_chunk(over));
    }

    #[test]
    fn verbosity_suppressed_after_tool_call() {
        let mut g = guard();
        g.on_tool_completed("shell", "echo hi");
        // After a tool call, verbosity guard is disabled for this turn.
        assert!(!g.on_text_chunk(VERBOSITY_SOFT_LIMIT_BYTES + 1));
    }

    fn progress_guard() -> AgentLoopGuard {
        AgentLoopGuard::new(0, false).with_progress_check()
    }

    /// `n` calls that make no progress: the same search, over and over
    /// with a different pattern each time.
    fn search_without_progress(g: &mut AgentLoopGuard, n: usize) -> Vec<ProgressCheck> {
        (0..n)
            .map(|i| g.on_tool_progress("search_code", &format!(r#"{{"pattern": "needle{i}"}}"#)))
            .collect()
    }

    #[test]
    fn a_window_without_progress_nudges_once_then_finalizes() {
        let mut g = progress_guard();
        let first = search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS);
        assert!(
            first[..PROGRESS_WINDOW_TOOL_CALLS - 1]
                .iter()
                .all(|c| *c == ProgressCheck::Fine)
        );
        let ProgressCheck::Nudge(nudge) = &first[PROGRESS_WINDOW_TOOL_CALLS - 1] else {
            panic!("expected a nudge, got {:?}", first.last());
        };
        assert!(nudge.contains("change approach"));
        assert!(nudge.contains("finish with your best result"));

        let second = search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS);
        assert_eq!(second.last(), Some(&ProgressCheck::Finalize));
        assert_eq!(
            second.iter().filter(|c| **c != ProgressCheck::Fine).count(),
            1,
            "one verdict per window"
        );
    }

    #[test]
    fn writes_new_tests_and_new_reads_are_progress() {
        let progress = [
            ("write_file", r#"{"path": "a.py", "content": "x"}"#),
            ("apply_diff", r#"{"path": "a.py"}"#),
            ("shell_execute", r#"{"command": "sed -i 's/a/b/' a.py"}"#),
            (
                "shell_execute",
                r#"{"command": "python -m pytest tests/test_a.py -q 2>&1 | tail -5"}"#,
            ),
            ("read_file", r#"{"path": "src/new.py"}"#),
            (
                "shell_execute",
                r#"{"command": "sed -n '1,80p' src/other.py"}"#,
            ),
            (
                "shell_execute",
                r#"{"command": "rg -n foo src | head; cat setup.cfg"}"#,
            ),
        ];
        for (name, input) in progress {
            let mut g = progress_guard();
            search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS - 1);
            // Without progress this call would complete the window.
            assert_eq!(
                g.on_tool_progress(name, input),
                ProgressCheck::Fine,
                "{input}"
            );
            assert!(
                search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS - 1)
                    .iter()
                    .all(|c| *c == ProgressCheck::Fine),
                "{input} restarts the window"
            );
        }
    }

    #[test]
    fn new_pages_queries_and_code_are_progress_repeats_are_not() {
        let mut g = progress_guard();
        let calls = [
            ("search_web", r#"{"query": "1928 Olympics flag bearers"}"#),
            ("fetch", r#"{"url": "https://en.wikipedia.org/wiki/A"}"#),
            (
                "fetch",
                r#"{"url": "https://en.wikipedia.org/wiki/A", "start_index": 5000}"#,
            ),
            ("browser_navigate", r#"{"url": "https://example.org/b"}"#),
            ("execute_code", r#"{"code": "print(sum(range(10)))"}"#),
        ];
        for (name, input) in calls {
            search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS - 1);
            assert_eq!(
                g.on_tool_progress(name, input),
                ProgressCheck::Fine,
                "{input}"
            );
        }
        // A research run that keeps reading new pages is never stopped.
        for i in 0..PROGRESS_WINDOW_TOOL_CALLS * 3 {
            let url = format!(r#"{{"url": "https://example.org/page{i}"}}"#);
            assert_eq!(g.on_tool_progress("fetch", &url), ProgressCheck::Fine);
        }
        // The same page, query or snippet again is not progress.
        let mut last = ProgressCheck::Fine;
        for i in 0..PROGRESS_WINDOW_TOOL_CALLS {
            let (name, input) = calls[i % calls.len()];
            last = g.on_tool_progress(name, input);
        }
        assert!(matches!(last, ProgressCheck::Nudge(_)), "{last:?}");
    }

    #[test]
    fn rereading_and_rerunning_are_not_progress() {
        let mut g = progress_guard();
        g.on_tool_progress("read_file", r#"{"path": "a.py"}"#);
        g.on_tool_progress("shell_execute", r#"{"command": "pytest -q"}"#);
        let mut last = ProgressCheck::Fine;
        for i in 0..PROGRESS_WINDOW_TOOL_CALLS {
            last = if i % 2 == 0 {
                g.on_tool_progress("read_file", r#"{"path": "a.py"}"#)
            } else {
                g.on_tool_progress("shell_execute", r#"{"command": "pytest -q"}"#)
            };
        }
        assert!(matches!(last, ProgressCheck::Nudge(_)), "{last:?}");
    }

    /// Whether `input` to `name`, as the call that would complete a window
    /// without progress, restarts the window instead.
    fn counts_as_progress(g: &mut AgentLoopGuard, name: &str, input: &str) -> bool {
        search_without_progress(g, PROGRESS_WINDOW_TOOL_CALLS - 1);
        let verdict = g.on_tool_progress(name, input);
        if verdict == ProgressCheck::Fine {
            true
        } else {
            // Start the next case from a clean window.
            *g = progress_guard().with_workspace_root(g.workspace_root.clone().as_deref());
            false
        }
    }

    fn shell(command: &str) -> String {
        serde_json::json!({ "command": command }).to_string()
    }

    #[test]
    fn scratch_writes_outside_the_tree_are_not_progress() {
        let root = Path::new("/work/repo");
        let mut g = progress_guard().with_workspace_root(Some(root));
        for command in [
            "cat > /tmp/repro.py <<'EOF'\nimport os\nprint(1 > 0)\nEOF",
            "cat > /tmp/dbg2.py << EOF\nwith open('src/app.py', 'w') as f:\n    f.write('x')\nEOF\npython /tmp/dbg2.py",
            "python /tmp/repro.py > /tmp/out.txt 2>&1",
            "echo hi >> /tmp/log.txt",
            "python -c \"open('/tmp/scratch.json', 'w').write('{}')\"",
            "python -c \"print(open('src/app.py').read())\"",
            "python -c \"import sys; sys.stdout.write('ok')\"",
            "python -c \"print(3 > 2)\"",
            "ls src > /dev/null",
            "tee /tmp/a.txt < input.txt",
            "cp src/app.py /tmp/app_backup.py",
        ] {
            assert!(
                !counts_as_progress(&mut g, "shell_execute", &shell(command)),
                "{command}"
            );
        }
        for (name, input) in [
            ("write_file", r#"{"path": "/tmp/check.py", "content": "x"}"#),
            (
                "write_file",
                r#"{"path": "/work/repo/../elsewhere/x.py", "content": "x"}"#,
            ),
            (
                "move_file",
                r#"{"source": "a.py", "destination": "/tmp/a.py"}"#,
            ),
        ] {
            assert!(!counts_as_progress(&mut g, name, input), "{input}");
        }
    }

    #[test]
    fn rewriting_the_same_scratch_script_never_resets_the_window() {
        let mut g = progress_guard().with_workspace_root(Some(Path::new("/work/repo")));
        let mut verdicts = Vec::new();
        for i in 0..PROGRESS_WINDOW_TOOL_CALLS {
            let command = if i % 2 == 0 {
                format!("cat > /tmp/repro.py <<'EOF'\nprint({i})\nEOF")
            } else {
                // Reading the scratch file back is no new read either.
                "cat /tmp/repro.py && python /tmp/repro.py".to_string()
            };
            verdicts.push(g.on_tool_progress("shell_execute", &shell(&command)));
        }
        assert!(
            matches!(verdicts.last(), Some(ProgressCheck::Nudge(_))),
            "{verdicts:?}"
        );
    }

    #[test]
    fn writes_inside_the_tree_are_progress() {
        let root = Path::new("/work/repo");
        let mut g = progress_guard().with_workspace_root(Some(root));
        for command in [
            "cat > src/fix.py <<'EOF'\nx = 1\nEOF",
            "cat <<EOF > /work/repo/tests/test_fix.py\nx = 1\nEOF",
            "echo x >> ./notes.txt",
            "sed -i 's/a/b/' /work/repo/src/app.py",
            "python - <<'EOF'\nwith open('src/app.py', 'w') as f:\n    f.write('x')\nEOF",
            "python -c \"from pathlib import Path; Path('src/app.py').write_text('x')\"",
            "python -c \"p = 'src/app.py'; open(p, 'w').write('x')\"",
            "cp /tmp/fixed.py src/app.py",
            "git apply /tmp/fix.patch",
            "echo done 2>&1 > out.txt",
        ] {
            assert!(
                counts_as_progress(&mut g, "shell_execute", &shell(command)),
                "{command}"
            );
        }
        for (name, input) in [
            ("write_file", r#"{"path": "src/app.py", "content": "x"}"#),
            (
                "write_file",
                r#"{"path": "/work/repo/src/app.py", "content": "x"}"#,
            ),
            ("apply_diff", r#"{"path": "src/app.py"}"#),
            ("final_answer", r#"{"answer": "42"}"#),
        ] {
            assert!(counts_as_progress(&mut g, name, input), "{input}");
        }
    }

    #[test]
    fn a_workspace_under_the_temp_dir_is_still_the_tree() {
        let mut g = progress_guard().with_workspace_root(Some(Path::new("/tmp/job/repo")));
        assert!(counts_as_progress(
            &mut g,
            "write_file",
            r#"{"path": "/tmp/job/repo/a.py", "content": "x"}"#
        ));
        assert!(!counts_as_progress(
            &mut g,
            "write_file",
            r#"{"path": "/tmp/job/scratch.py", "content": "x"}"#
        ));
    }

    #[test]
    fn without_a_workspace_root_only_scratch_dirs_are_outside() {
        let mut g = progress_guard();
        assert!(counts_as_progress(
            &mut g,
            "shell_execute",
            &shell("echo x > /srv/app/a.py")
        ));
        assert!(!counts_as_progress(
            &mut g,
            "shell_execute",
            &shell("echo x > /tmp/a.py")
        ));
        assert!(!counts_as_progress(
            &mut g,
            "shell_execute",
            &shell("echo x > /var/tmp/a.py")
        ));
    }

    #[test]
    fn shell_writes_reads_redirections_and_python_writes() {
        let w = shell_writes("python run.py 2>/dev/null >> log.txt && echo '> not.txt' 1>&2");
        assert_eq!(
            w.targets,
            vec!["/dev/null".to_string(), "log.txt".to_string()]
        );
        assert!(!w.untargeted);
        let w = shell_writes("python -c \"open('a.txt', mode='a').write('x'); open('b.txt')\"");
        assert_eq!(w.targets, vec!["a.txt".to_string()]);
        let w = shell_writes("sed -i -e 's/a/b/' x.py y.py");
        assert_eq!(w.targets, vec!["x.py".to_string(), "y.py".to_string()]);
        assert_eq!(
            shell_writes("grep -rn foo src | head"),
            ShellWrites::default()
        );
    }

    #[test]
    fn the_progress_check_is_off_unless_asked_for() {
        let mut g = AgentLoopGuard::new(0, false);
        let checks = search_without_progress(&mut g, PROGRESS_WINDOW_TOOL_CALLS * 3);
        assert!(checks.iter().all(|c| *c == ProgressCheck::Fine));
    }

    #[test]
    fn shell_read_paths_finds_the_files_read() {
        assert_eq!(
            shell_read_paths("sed -n '10,40p' src/a.py && cat README.md | head -5"),
            vec!["src/a.py".to_string(), "README.md".to_string()]
        );
        assert!(shell_read_paths("rg -n needle src").is_empty());
    }

    #[test]
    fn verbosity_resets_on_turn_complete() {
        let mut g = guard();
        g.on_text_chunk(VERBOSITY_SOFT_LIMIT_BYTES + 1);
        g.on_turn_complete(1, false);
        // Fresh turn: counter reset, no trigger until limit exceeded again.
        assert!(!g.on_text_chunk(100));
    }
}
