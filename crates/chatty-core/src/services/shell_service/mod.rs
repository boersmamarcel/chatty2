//! `shell_service` — persistent shell session for the agent.
//!
//! Runs a long-lived bash on a PTY (through `chatty-terminal`) and executes
//! the agent's commands in it. Used by the shell tool when the user enables
//! local execution.
//!
//! # What lives here
//!
//! - `ShellSession` — owns the shell's [`TerminalHandle`] and what the byte
//!   tap has read from it: prompt state, completion marks, command output.
//! - Spawning (bash, inside bubblewrap on Linux or sandbox-exec on macOS
//!   when available), the shell's init, timeouts, and shutdown.
//!
//! # How a command runs
//!
//! The shell's prompt marks itself with OSC 133 `A`/`B`, `PS0` prints `C`
//! when a command starts and `PROMPT_COMMAND` prints `D;<exit>` when it
//! ends. An agent command also carries an id, sent through a key binding
//! rather than typed, so its `C` and `D` are followed by chatty's private
//! OSC 6973 with that id. Nothing of this is visible in a terminal view: it
//! shows a normal prompt and the command. The model's result is the output
//! between the two id marks, read from the byte stream as clean text (see
//! [`chatty_terminal::CleanText`]), not from the grid.
//!
//! # What does NOT live here
//!
//! - The user-facing shell tool — `tools::shell_tool` (registers this
//!   service with the agent and shapes its tool calls).
//! - Approval prompts — `models::execution_approval_store`.
//! - Sandboxed execution (Docker / Daytona) — `sandbox/` and `tools::daytona_tool`.

use anyhow::{Result, anyhow};
use chatty_terminal::{Mark, MarkScanner, TerminalConfig, TerminalEvent, TerminalHandle};
use serde::Serialize;
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tokio::sync::{Mutex, Notify};
use tokio::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Output from a shell command execution
#[derive(Debug, Serialize)]
pub struct ShellOutput {
    pub stdout: String,
    pub exit_code: i32,
    pub truncated: bool,
    /// True when the command hit the timeout and was killed. `stdout` still
    /// carries whatever output was captured before the kill, followed by a
    /// note saying so, rather than being discarded.
    pub timed_out: bool,
}

/// Upper bound on the per-call `timeout_seconds` a model can request via
/// `shell_execute`, regardless of the configured default. Prevents a single
/// tool call from blocking a turn indefinitely.
pub const MAX_SHELL_CALL_TIMEOUT_SECONDS: u32 = 600;

/// Shell output the model sees whole. Longer output keeps its head and tail
/// around an omission line, whatever larger `max_output_bytes` is configured
/// (a smaller one still wins). 8 KB is about 2k tokens: a whole 51 KB dump,
/// the old cap, filled a sixth of a 32k window in one call, and on SWE-bench
/// runs the model paged through such dumps instead of filtering them.
pub const SHELL_OUTPUT_DEFAULT_CAP_BYTES: usize = 8_192;

/// Current status of the shell session
#[derive(Debug, Serialize)]
pub struct ShellStatus {
    pub running: bool,
    pub cwd: String,
    pub env_vars: Vec<(String, String)>,
    pub pid: Option<u32>,
    pub uptime_seconds: u64,
}

/// The running shell.
struct ShellProcess {
    terminal: Arc<TerminalHandle>,
    tap: Arc<Tap>,
    is_sandboxed: bool,
}

impl ShellProcess {
    /// Kill the shell (its whole process group) now, whoever else still
    /// holds the terminal.
    fn kill(&self) {
        self.terminal.kill();
    }
}

/// How long the login-profile init may take before the session gives up on
/// it and runs without the profile (see [`LOGIN_PROFILE_INIT`]). Also bounds
/// the wait for the first prompt of a shell started without it.
const LOGIN_PROFILE_TIMEOUT: Duration = Duration::from_secs(10);

/// Loads what a login shell (`bash -l`, the way other agent harnesses run
/// commands) would: `/etc/profile`, then the first of `~/.bash_profile`,
/// `~/.bash_login`, `~/.profile` (Debian/Ubuntu's `~/.profile` sources
/// `~/.bashrc` in turn). The shell itself starts with `--norc --noprofile`
/// and runs this before its first prompt instead (see [`shell_init`]), so
/// output the profile prints stays out of the terminal, a `read` in it
/// cannot take keystrokes, and a hang is bounded: its stdin is `/dev/null`,
/// its output is discarded, `set -e`/`-u` it may leave behind are undone
/// (either would end the persistent shell on the model's first failing
/// command), as is `set -x`/`-v` (the trace would land in every command's
/// output), the starting directory is restored, and the caller bounds the
/// whole thing with [`LOGIN_PROFILE_TIMEOUT`].
///
/// `PATH` entries the shell inherited but the profile dropped are appended
/// back: Debian's `/etc/profile` resets `PATH` to the system directories,
/// which would otherwise lose a container's `ENV PATH` additions
/// (`/usr/local/cargo/bin`, `/usr/local/go/bin`, a venv). What the profile
/// prepends still comes first.
///
/// Without it the project's environment — a conda env activated in the
/// login profile, PATH additions — was missing: "No module named …" in
/// 20/20 SWE-bench trials, 3–10 turns lost each.
const LOGIN_PROFILE_INIT: &str = r#"__chatty_cwd=$PWD
__chatty_path=$PATH
{ if [ -r /etc/profile ]; then . /etc/profile; fi
for __chatty_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
if [ -r "$__chatty_f" ]; then . "$__chatty_f"; break; fi
done; } </dev/null >/dev/null 2>&1
set +euxv
cd "$__chatty_cwd" 2>/dev/null
IFS=: read -r -a __chatty_pa <<<"$__chatty_path"
for __chatty_p in "${__chatty_pa[@]}"; do
case ":$PATH:" in *":$__chatty_p:"*) ;; *) [ -n "$__chatty_p" ] && PATH=${PATH:+$PATH:}$__chatty_p ;; esac
done
export PATH
unset __chatty_cwd __chatty_f __chatty_path __chatty_pa __chatty_p
"#;

/// The rest of the shell's init, after the profile and the secrets.
///
/// The shell is interactive (it has a terminal), which a profile or a
/// command could notice; this keeps what the model's commands see as close
/// to the old non-interactive shell as a terminal allows: no job control
/// (every command stays in the shell's process group, so a timeout's kill
/// takes it down, and no job notices), no `!` history expansion (it would
/// rewrite `echo "hi!"`), no aliases from the profile (`rm -i`; an
/// interactive bash expands them whatever `expand_aliases` says), no history
/// file, no auto-logout, no command-not-found suggestions, and pagers that
/// print instead of waiting for a key.
///
/// Then the marks: `\e[6973~` is bound to a function that takes the line
/// typed so far as the next command's id (the session pastes the id, then
/// that key, then the command), `PS0` prints `C` (plus the id mark),
/// `PROMPT_COMMAND` prints `D` (plus the id mark), and `PS1` wraps a plain
/// prompt in `A`/`B`. The last line says the init is done.
const SHELL_INIT: &str = r#"set +m +H
shopt -u expand_aliases
unalias -a
unset HISTFILE MAILCHECK TMOUT ALACRITTY_WINDOW_ID WINDOWID __CHATTY_INIT PROMPT_COMMAND
unset -f command_not_found_handle
trap - DEBUG
export PAGER=cat GIT_PAGER=cat MANPAGER=cat LESS=-FRX
__chatty_take_id() {
__chatty_id=$READLINE_LINE
__chatty_ps0=$'\e]133;C\a\e]6973;C;'"$__chatty_id"$'\a'
READLINE_LINE=
READLINE_POINT=0
}
__chatty_prompt() {
local ec=$?
printf '\e]133;D;%s\a' "$ec"
if [ -n "${__chatty_id-}" ]; then printf '\e]6973;D;%s;%s\a' "$__chatty_id" "$ec"; __chatty_id=; fi
__chatty_ps0=$'\e]133;C\a'
}
bind 'set enable-bracketed-paste on'
bind -m emacs -x '"\e[6973~": __chatty_take_id'
bind -m vi-insert -x '"\e[6973~": __chatty_take_id'
__chatty_ps0=$'\e]133;C\a'
PS0='${__chatty_ps0}'
PS1='\[\e]133;A\a\]\w\$ \[\e]133;B\a\]'
PS2='> '
PROMPT_COMMAND=__chatty_prompt
export -n PROMPT_COMMAND
printf '\e]6973;ready\a'
"#;

/// The key bound to `__chatty_take_id` in [`SHELL_INIT`].
const TAKE_ID_KEY: &[u8] = b"\x1b[6973~";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Terminal size when no view is attached: wide, so programs that fit their
/// output to the terminal (`ls`, `git`, test runners) don't wrap it early.
const HEADLESS_COLS: u16 = 200;
const HEADLESS_ROWS: u16 = 50;

/// How long an agent command waits for the terminal to be back at an empty
/// prompt (someone typing, a command started from a view) before giving up.
const BUSY_WAIT: Duration = Duration::from_secs(2);

/// Build the init the shell runs before its first prompt: the login profile
/// (when `load_login_profile`), then the user's secrets as exports, so a
/// profile cannot override them, then [`SHELL_INIT`].
fn shell_init(load_login_profile: bool, secrets: &[(String, String)]) -> String {
    let mut init = String::new();
    if load_login_profile {
        // Without a `PS1` the profile takes the non-interactive branches the
        // old piped shell took (`/etc/profile` skips `/etc/bash.bashrc` and
        // bash-completion, a stock root `.bashrc` returns early); the init
        // sets the prompt afterwards.
        init.push_str("unset PS1\n");
        init.push_str(LOGIN_PROFILE_INIT);
    }
    if !secrets.is_empty() {
        let secret_keys: Vec<&str> = secrets.iter().map(|(k, _)| k.as_str()).collect();
        info!(keys = ?secret_keys, "Injecting user secrets into shell session");
    }
    for (key, value) in secrets {
        // Validate key (same rules as set_env)
        if key.chars().all(|c| c.is_alphanumeric() || c == '_') && !key.is_empty() {
            let escaped_value = value.replace('\'', "'\\''");
            init.push_str(&format!("export {}='{}'\n", key, escaped_value));
        } else {
            warn!(key = %key, "Skipping invalid secret key name");
        }
    }
    init.push_str(SHELL_INIT);
    init
}

/// What the byte tap has read from the shell, shared between the PTY
/// thread and the session.
struct Tap {
    state: std::sync::Mutex<TapState>,
    /// Woken whenever `state` may have changed.
    changed: Notify,
}

impl Tap {
    fn lock(&self) -> std::sync::MutexGuard<'_, TapState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut TapState)) {
        f(&mut self.lock());
        self.changed.notify_waiters();
    }

    /// Wait until `check` returns something, or `deadline` passes.
    async fn wait_for<T>(
        &self,
        deadline: Instant,
        mut check: impl FnMut(&mut TapState) -> Option<T>,
    ) -> Option<T> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(value) = check(&mut self.lock()) {
                return Some(value);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return check(&mut self.lock());
            }
        }
    }
}

/// Where the shell is, read from its marks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    /// Nothing seen yet, or between a command's end and the next prompt.
    Between,
    /// Printing the prompt (after `A`, before `B`).
    Printing,
    /// At the prompt, reading a command line (after `B`).
    Reading,
    /// Running the command typed at the prompt (after `C`).
    Running { command_line: String },
}

struct AgentCommand {
    id: String,
    /// Output is being kept (its start mark was seen).
    capturing: bool,
    /// Output and exit code, once its end mark was seen.
    done: Option<(String, i32)>,
}

struct TapState {
    scanner: MarkScanner,
    prompt: Prompt,
    /// The init finished.
    ready: bool,
    command: Option<AgentCommand>,
    /// The shell exited, with this code.
    exited: Option<i32>,
}

impl TapState {
    fn new() -> Self {
        let mut scanner = MarkScanner::new();
        // Keep what the shell prints before its first prompt: if it fails
        // to start (a sandbox error), that is the reason.
        scanner.text_mut().set_enabled(true);
        Self {
            scanner,
            prompt: Prompt::Between,
            ready: false,
            command: None,
            exited: None,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        let Self {
            scanner,
            prompt,
            ready,
            command,
            ..
        } = self;
        scanner.feed(bytes, |mark, text| {
            let capturing = command
                .as_ref()
                .is_some_and(|c| c.capturing && c.done.is_none());
            match mark {
                Mark::PromptStart => {
                    *prompt = Prompt::Printing;
                    if !capturing {
                        // Keep the prompt, to know where the command line
                        // starts on its row.
                        text.set_enabled(false);
                        text.set_enabled(true);
                    }
                }
                Mark::CommandStart => {
                    *prompt = Prompt::Reading;
                    if !capturing {
                        // From here on, keep what is typed at the prompt
                        // (to tell whether someone is typing, and what
                        // they ran). Readline redraws relative to the
                        // prompt's width, so start after it.
                        let shown = text.take();
                        let width = shown.rsplit('\n').next().unwrap_or("").chars().count();
                        text.set_enabled(true);
                        text.move_to_column(width);
                    }
                }
                Mark::OutputStart => {
                    let typed = if capturing {
                        String::new()
                    } else {
                        text.take()
                    };
                    let command_line = typed
                        .lines()
                        .rev()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or_default()
                        .to_string();
                    *prompt = Prompt::Running { command_line };
                    if !capturing {
                        text.set_enabled(false);
                    }
                }
                Mark::CommandEnd { .. } => {
                    *prompt = Prompt::Between;
                    if !capturing {
                        text.set_enabled(false);
                    }
                }
                Mark::Private(fields) => {
                    let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
                    match (fields.as_slice(), command.as_mut()) {
                        (["ready"], _) => *ready = true,
                        // `PS0` shows again for each command of a
                        // multi-line command line: the first one starts it.
                        (["C", id], Some(cmd)) if *id == cmd.id && !cmd.capturing => {
                            cmd.capturing = true;
                            text.set_enabled(false);
                            text.set_enabled(true);
                        }
                        (["D", id, code], Some(cmd)) if *id == cmd.id && cmd.capturing => {
                            let code = code.parse().unwrap_or(-1);
                            cmd.done = Some((text.take(), code));
                            text.set_enabled(false);
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    /// Why an agent command can't be sent now, or `None` when the shell is
    /// at an empty prompt.
    fn busy(&self) -> Option<String> {
        match &self.prompt {
            Prompt::Reading if self.scanner.text().is_blank() => None,
            Prompt::Reading => Some("someone is typing at the prompt".to_string()),
            Prompt::Running { command_line } if !command_line.is_empty() => {
                Some(format!("`{command_line}` is running"))
            }
            Prompt::Running { .. } => Some("a command is running".to_string()),
            Prompt::Between | Prompt::Printing => {
                Some("the shell is not at its prompt".to_string())
            }
        }
    }
}

/// A persistent shell session that maintains state across multiple commands.
///
/// The session keeps a bash process alive on a PTY, preserving environment
/// variables, working directory, and other shell state between invocations.
///
/// Security: When sandboxing is available (bubblewrap on Linux, sandbox-exec on macOS),
/// the shell process runs inside a sandbox with filesystem and network restrictions.
/// Network isolation is controlled by the `network_isolation` setting.
pub struct ShellSession {
    process: Mutex<Option<ShellProcess>>,
    /// The running shell's terminal, readable while a command holds
    /// `process` (a view attaching, Ctrl+C from one).
    terminal: std::sync::Mutex<Option<Arc<TerminalHandle>>>,
    workspace_dir: Option<String>,
    network_isolation: bool,
    timeout_seconds: u32,
    max_output_bytes: usize,
    created_at: SystemTime,
    /// Environment variables injected on shell startup (user secrets).
    /// Re-injected on every respawn so secrets survive shell restarts.
    startup_env_vars: Vec<(String, String)>,
    /// Key names of user secrets, for masking in status output.
    secret_key_names: Vec<String>,
    /// Whether a (re)spawned shell loads the login profile. Cleared once a
    /// profile hangs or kills the shell, so a respawn doesn't pay for it
    /// again.
    load_login_profile: AtomicBool,
    /// `HOME` for the shell when set; tests point it at a scratch profile.
    home_override: Option<String>,
}

impl ShellSession {
    /// Create a new shell session with user secrets that will be injected
    /// as environment variables on every shell (re)start.
    ///
    /// The bash process is not spawned until the first command is executed.
    /// When spawned, it will run inside a sandbox if available (bubblewrap on Linux,
    /// sandbox-exec on macOS). Pass an empty `secrets` vec for no env injection.
    pub fn with_secrets(
        workspace_dir: Option<String>,
        timeout_seconds: u32,
        max_output_bytes: usize,
        network_isolation: bool,
        secrets: Vec<(String, String)>,
    ) -> Self {
        let secret_key_names = secrets.iter().map(|(k, _)| k.clone()).collect();
        Self {
            process: Mutex::new(None),
            terminal: std::sync::Mutex::new(None),
            workspace_dir,
            network_isolation,
            timeout_seconds,
            max_output_bytes,
            created_at: SystemTime::now(),
            startup_env_vars: secrets,
            secret_key_names,
            load_login_profile: AtomicBool::new(true),
            home_override: None,
        }
    }

    /// Run the shell with `HOME` set to `home` (tests: a scratch profile).
    #[cfg(test)]
    fn with_home(mut self, home: &str) -> Self {
        self.home_override = Some(home.to_string());
        self
    }

    /// Return the key names of user secrets (for masking in tool output).
    pub fn secret_key_names(&self) -> &[String] {
        &self.secret_key_names
    }

    /// The terminal the shell runs in, while one is running: for a view to
    /// attach to (it may resize it and type into it) or a reader to take
    /// snapshots of. A respawned shell has a new one.
    pub fn terminal(&self) -> Option<Arc<TerminalHandle>> {
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_terminal(&self, terminal: Option<Arc<TerminalHandle>>) {
        *self.terminal.lock().unwrap_or_else(|e| e.into_inner()) = terminal;
    }

    /// Check if sandboxing is available on this platform
    pub fn can_sandbox() -> bool {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("bwrap")
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        #[cfg(target_os = "macos")]
        {
            true
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }

    /// Return the network_isolation setting this session was created with
    pub fn network_isolation(&self) -> bool {
        self.network_isolation
    }

    /// Return the workspace directory this session was created with
    pub fn workspace_dir(&self) -> Option<&String> {
        self.workspace_dir.as_ref()
    }

    /// Cut `output` to its head and tail when it is over the effective cap:
    /// the smaller of [`SHELL_OUTPUT_DEFAULT_CAP_BYTES`] and the configured
    /// `max_output_bytes`. The middle gives way, not one end: a build or test
    /// run says what it is doing at the top and how it ended at the bottom.
    /// The omission line tells the model how to get the rest. Returns whether
    /// anything was cut.
    fn bound_output(output: &mut String, max_output_bytes: usize) -> bool {
        let cap = max_output_bytes.min(SHELL_OUTPUT_DEFAULT_CAP_BYTES);
        if output.len() <= cap {
            return false;
        }
        let mut head_end = cap / 2;
        while head_end > 0 && !output.is_char_boundary(head_end) {
            head_end -= 1;
        }
        let mut tail_start = output.len() - (cap - cap / 2);
        while tail_start < output.len() && !output.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let omitted = tail_start - head_end;
        *output = format!(
            "{}\n... [{omitted} bytes omitted; rerun with a filter (grep/tail) to see more] ...\n{}",
            &output[..head_end],
            &output[tail_start..]
        );
        true
    }

    fn exit_code_from_status(status: std::process::ExitStatus) -> i32 {
        status.code().unwrap_or(-1)
    }

    /// Check if the current session process is running inside a sandbox
    pub async fn is_sandboxed(&self) -> bool {
        let process = self.process.lock().await;
        process
            .as_ref()
            .map_or(Self::can_sandbox(), |p| p.is_sandboxed)
    }

    /// Ensure the shell is running, spawning it if necessary.
    ///
    /// Attempts to spawn inside a sandbox (bubblewrap on Linux, sandbox-exec on macOS).
    /// Falls back to unsandboxed execution if sandboxing is unavailable.
    /// The shell runs its init ([`shell_init`]: the login profile, the
    /// secrets, the marks) before its first prompt; a login profile that
    /// hangs or ends the shell respawns it without the profile.
    async fn ensure_started(&self, process: &mut Option<ShellProcess>) -> Result<()> {
        if let Some(proc) = process.as_ref() {
            match proc.tap.lock().exited {
                None => return Ok(()), // Still running
                Some(code) => {
                    warn!(
                        exit_code = code,
                        "Shell process exited unexpectedly, respawning"
                    );
                }
            }
            *process = None;
            self.set_terminal(None);
        }

        info!(workspace = ?self.workspace_dir, "Spawning persistent shell session");

        loop {
            let load_login_profile = self.load_login_profile.load(Ordering::Relaxed);
            let init = shell_init(load_login_profile, &self.startup_env_vars);
            let proc = Self::spawn_shell(
                &self.workspace_dir,
                self.network_isolation,
                self.home_override.as_deref(),
                &init,
            )?;
            match Self::wait_ready(&proc).await {
                Ok(()) => {
                    info!(pid = ?proc.terminal.pid(), sandboxed = proc.is_sandboxed, "Shell session started");
                    self.set_terminal(Some(Arc::clone(&proc.terminal)));
                    *process = Some(proc);
                    return Ok(());
                }
                Err(e) if load_login_profile => {
                    // A profile that hangs, exits or execs: run this session
                    // without it rather than lose the shell.
                    warn!(error = %e, "Login profile did not load; continuing without it");
                    self.load_login_profile.store(false, Ordering::Relaxed);
                    proc.kill();
                }
                Err(e) => {
                    proc.kill();
                    return Err(e);
                }
            }
        }
    }

    /// Wait, up to [`LOGIN_PROFILE_TIMEOUT`], for a freshly spawned shell to
    /// finish its init and show its first prompt. An error means the shell
    /// is unusable (hung, exited) and must be killed.
    async fn wait_ready(proc: &ShellProcess) -> Result<()> {
        let deadline = Instant::now() + LOGIN_PROFILE_TIMEOUT;
        let outcome = proc
            .tap
            .wait_for(deadline, |state| {
                if let Some(code) = state.exited {
                    let said = state.scanner.text_mut().take();
                    let said = said.trim();
                    return Some(Err(if said.is_empty() {
                        anyhow!("the shell exited before its first prompt (exit code {code})")
                    } else {
                        anyhow!(
                            "the shell exited before its first prompt (exit code {code}): {said}"
                        )
                    }));
                }
                (state.ready && state.prompt == Prompt::Reading).then_some(Ok(()))
            })
            .await;
        outcome.unwrap_or_else(|| {
            Err(anyhow!(
                "the shell's init took longer than {}s",
                LOGIN_PROFILE_TIMEOUT.as_secs()
            ))
        })
    }

    /// Spawn the shell: inside a sandbox when one is available, else (or
    /// when the sandboxed spawn fails) unsandboxed.
    fn spawn_shell(
        workspace_dir: &Option<String>,
        network_isolation: bool,
        home: Option<&str>,
        init: &str,
    ) -> Result<ShellProcess> {
        if Self::can_sandbox() {
            match Self::sandboxed_config(workspace_dir, network_isolation)
                .and_then(|config| Self::spawn_terminal(config, home, init, true))
            {
                Ok(proc) => {
                    info!("Shell session spawned inside sandbox");
                    return Ok(proc);
                }
                Err(e) => {
                    warn!(error = ?e, "Sandboxed shell spawn failed, falling back to unsandboxed");
                }
            }
        } else {
            info!("Sandboxing not available, spawning unsandboxed shell session");
        }
        let config = TerminalConfig {
            shell: Some("/bin/bash".to_string()),
            args: bash_args(),
            cwd: workspace_dir.as_ref().map(Into::into),
            ..TerminalConfig::default()
        };
        Self::spawn_terminal(config, home, init, false)
    }

    /// Start `config` on a PTY with the agent's environment and the byte tap.
    fn spawn_terminal(
        mut config: TerminalConfig,
        home: Option<&str>,
        init: &str,
        is_sandboxed: bool,
    ) -> Result<ShellProcess> {
        config.size = (HEADLESS_COLS, HEADLESS_ROWS);
        config.env.extend(agent_env());
        if let Some(home) = home {
            config.env.insert("HOME".into(), home.into());
        }
        // The init runs as the first prompt command, so nothing is typed
        // into the terminal to start the shell.
        config.env.insert("__CHATTY_INIT".into(), init.into());
        config
            .env
            .insert("PROMPT_COMMAND".into(), "eval \"$__CHATTY_INIT\"".into());

        let tap = Arc::new(Tap {
            state: std::sync::Mutex::new(TapState::new()),
            changed: Notify::new(),
        });
        let (terminal, events) = TerminalHandle::spawn_with_tap(config, {
            let tap = Arc::clone(&tap);
            move |bytes| tap.update(|state| state.feed(bytes))
        })
        .map_err(|e| anyhow!("Failed to spawn shell process: {}", e))?;
        watch_exit(events, Arc::clone(&tap));

        Ok(ShellProcess {
            terminal: Arc::new(terminal),
            tap,
            is_sandboxed,
        })
    }

    /// The sandbox wrapper as the PTY's program (Linux: bubblewrap, macOS:
    /// sandbox-exec), running bash.
    ///
    /// The persistent bash process runs inside the sandbox, inheriting all
    /// restrictions (filesystem isolation, network isolation). State (env vars,
    /// cwd) is maintained within the sandboxed process between commands.
    ///
    /// The login profile loads inside the sandbox too, so it only finds what
    /// the sandbox exposes (bubblewrap binds neither `/etc` nor `HOME`).
    fn sandboxed_config(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<TerminalConfig> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self::sandboxed_config_linux(
                workspace_dir,
                network_isolation,
            ))
        }

        #[cfg(target_os = "macos")]
        {
            Self::sandboxed_config_macos(workspace_dir, network_isolation)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (workspace_dir, network_isolation);
            Err(anyhow!("Sandboxing not supported on this platform"))
        }
    }

    /// bubblewrap around bash on Linux.
    ///
    /// No `--new-session`: it would `setsid()` bash away from the PTY, and
    /// without a controlling terminal Ctrl+C and job control stop working.
    /// What it guards against, a sandboxed process pushing keystrokes into
    /// the terminal outside the sandbox with `TIOCSTI`, has no target here:
    /// the controlling terminal is this private PTY, whose only reader is
    /// the sandboxed bash itself.
    #[cfg(target_os = "linux")]
    fn sandboxed_config_linux(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> TerminalConfig {
        let mut args: Vec<String> = [
            // Bind essential system directories as read-only
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/lib",
            "/lib",
            "--ro-bind",
            "/bin",
            "/bin",
            "--ro-bind",
            "/sbin",
            "/sbin",
            "--tmpfs",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            // --unshare-all includes --unshare-net; we re-enable network
            // below with --share-net when network_isolation is false.
            "--unshare-all",
            "--die-with-parent",
        ]
        .map(String::from)
        .to_vec();

        // Re-enable network access when isolation is not requested
        if !network_isolation {
            args.push("--share-net".into());
        }

        // Check for /lib64 (exists on many 64-bit Linux systems)
        if std::path::Path::new("/lib64").exists() {
            args.extend(["--ro-bind", "/lib64", "/lib64"].map(String::from));
        }

        // Bind workspace at its original path so existing path references work
        if let Some(workspace) = workspace_dir {
            args.extend(["--bind", workspace, workspace].map(String::from));
            args.extend(["--chdir", workspace].map(String::from));
        }

        args.push("/bin/bash".into());
        args.extend(bash_args());
        TerminalConfig {
            shell: Some("bwrap".to_string()),
            args,
            ..TerminalConfig::default()
        }
    }

    /// sandbox-exec around bash on macOS.
    #[cfg(target_os = "macos")]
    fn sandboxed_config_macos(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<TerminalConfig> {
        let profile = Self::build_macos_sandbox_profile(workspace_dir, network_isolation)?;

        let mut args = vec!["-p".to_string(), profile, "/bin/bash".to_string()];
        args.extend(bash_args());
        Ok(TerminalConfig {
            shell: Some("sandbox-exec".to_string()),
            args,
            cwd: workspace_dir.as_ref().map(Into::into),
            // In sandboxed sessions we only allow writes under /tmp by default.
            // Point TMPDIR to /tmp so tools that rely on temporary files (e.g. `uv`)
            // don't attempt writes to /var/folders/... and fail with EPERM.
            env: HashMap::from([("TMPDIR".to_string(), "/tmp".to_string())]),
            ..TerminalConfig::default()
        })
    }
    /// Build the macOS sandbox profile (SBPL)
    ///
    /// - Allows default operations
    /// - Denies writes to sensitive system directories
    /// - Denies reads to credential files (.ssh, .aws, etc.)
    /// - Denies network access when `network_isolation` is true
    /// - Allows writes to /tmp and workspace
    #[cfg(target_os = "macos")]
    fn build_macos_sandbox_profile(
        workspace_dir: &Option<String>,
        network_isolation: bool,
    ) -> Result<String> {
        let workspace_write_rule = if let Some(workspace) = workspace_dir {
            let safe_workspace = Self::escape_sandbox_path(workspace)?;
            format!(
                "\n                (allow file-write* (subpath \"{}\"))",
                safe_workspace
            )
        } else {
            String::new()
        };

        let network_rule = if network_isolation {
            "\n                ;; Deny network access (network isolation enabled)\n                (deny network*)"
        } else {
            "\n                ;; Network access allowed (network isolation disabled)"
        };

        Ok(format!(
            r#"
                (version 1)
                (allow default)

                ;; Deny write access to sensitive system directories
                (deny file-write*
                    (subpath "/System")
                    (subpath "/Library")
                    (subpath "/private/etc")
                    (subpath "/private/var")
                    (regex #"^/Users/[^/]+/\.ssh")
                    (regex #"^/Users/[^/]+/\.aws")
                    (regex #"^/Users/[^/]+/\.gnupg")
                )

                ;; Deny read access to sensitive credential files and directories
                (deny file-read*
                    (regex #"^/Users/[^/]+/\.ssh/")
                    (regex #"^/Users/[^/]+/\.aws/")
                    (regex #"^/Users/[^/]+/\.gnupg/")
                    (regex #"^/Users/[^/]+/\.docker/config\.json$")
                    (regex #"^/Users/[^/]+/\.kube/config$")
                    (regex #"^/Users/[^/]+/\.netrc$")
                    (subpath "/private/etc/ssh")
                    (literal "/etc/master.passwd")
                    (literal "/etc/shadow")
                )
                {}
                (allow file-write* (subpath "/tmp")){}
            "#,
            network_rule, workspace_write_rule
        ))
    }

    /// Validate and escape a workspace path for safe use in macOS sandbox profile.
    ///
    /// Prevents path injection attacks by:
    /// 1. Validating the path is absolute
    /// 2. Rejecting paths with suspicious characters (parentheses)
    /// 3. Canonicalizing to resolve symlinks and relative components
    /// 4. Escaping special characters (quotes, backslashes)
    #[cfg(target_os = "macos")]
    fn escape_sandbox_path(workspace: &str) -> Result<String> {
        use std::path::Path;

        let path = Path::new(workspace);
        if !path.is_absolute() {
            return Err(anyhow!(
                "Workspace path must be absolute, got: {}",
                workspace
            ));
        }

        if workspace.contains('(') || workspace.contains(')') {
            return Err(anyhow!(
                "Workspace path contains invalid characters (parentheses): {}",
                workspace
            ));
        }

        let canonical = path.canonicalize().map_err(|e| {
            anyhow!(
                "Failed to canonicalize workspace path '{}': {}",
                workspace,
                e
            )
        })?;

        let canonical_str = canonical
            .to_str()
            .ok_or_else(|| anyhow!("Workspace path contains invalid UTF-8"))?;

        let escaped = canonical_str.replace('\\', "\\\\").replace('"', "\\\"");

        if escaped.contains('(') || escaped.contains(')') {
            return Err(anyhow!(
                "Canonicalized workspace path contains invalid characters: {}",
                canonical_str
            ));
        }

        Ok(escaped)
    }

    /// Execute a command in the persistent shell session, using the
    /// session's configured default timeout.
    ///
    /// The command's stdout and stderr both go to the terminal, so they are
    /// merged. Returns the combined output and exit code.
    pub async fn execute(&self, command: &str) -> Result<ShellOutput> {
        self.execute_with_timeout(command, None).await
    }

    /// Execute a command in the persistent shell session, optionally
    /// overriding the session's configured default timeout for this one
    /// call. The override is bounded by [`MAX_SHELL_CALL_TIMEOUT_SECONDS`].
    ///
    /// The command's stdout and stderr both go to the terminal, so they are
    /// merged. Returns the combined output and exit code. If the command
    /// times out, this does not error: it returns whatever output was
    /// captured before the session was killed, with a note appended saying
    /// the command timed out (`timed_out: true`) so the caller sees the
    /// partial progress instead of losing it.
    ///
    /// The command is only sent when the shell is at an empty prompt. If
    /// someone is typing at it or running something from a terminal view,
    /// this waits up to [`BUSY_WAIT`] and then fails saying so; it never
    /// types into a running program.
    pub async fn execute_with_timeout(
        &self,
        command: &str,
        timeout_override: Option<u32>,
    ) -> Result<ShellOutput> {
        let effective_timeout_seconds =
            resolve_call_timeout_seconds(self.timeout_seconds, timeout_override);

        let mut process = self.process.lock().await;
        self.ensure_started(&mut process).await?;

        // SAFETY: ensure_started() guarantees process is Some on Ok return
        let proc = process.as_mut().unwrap();

        let busy = proc
            .tap
            .wait_for(Instant::now() + BUSY_WAIT, |state| {
                state.busy().is_none().then_some(())
            })
            .await;
        if busy.is_none() {
            let reason = proc.tap.lock().busy().unwrap_or_default();
            return Err(anyhow!(
                "the terminal is busy: {reason}. The command was not sent; \
                 run it again once the shell is back at its prompt."
            ));
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        proc.tap.lock().command = Some(AgentCommand {
            id: id.clone(),
            capturing: false,
            done: None,
        });

        // Paste the id and hand it to the shell with the bound key, then
        // paste the command and accept it. A paste arrives as one line, so
        // a multi-line command runs as one unit (one `C`, one `D`, the last
        // command's exit code) and a tab in it is not a completion request.
        let mut input = Vec::with_capacity(command.len() + 64);
        input.extend_from_slice(PASTE_START);
        input.extend_from_slice(id.as_bytes());
        input.extend_from_slice(PASTE_END);
        input.extend_from_slice(TAKE_ID_KEY);
        input.extend_from_slice(PASTE_START);
        input.extend_from_slice(command.replace("\x1b[201~", "").as_bytes());
        input.extend_from_slice(PASTE_END);
        input.push(b'\r');
        proc.terminal
            .write(&input)
            .map_err(|e| anyhow!("Failed to write to the shell's terminal: {}", e))?;

        let deadline = Instant::now() + Duration::from_secs(effective_timeout_seconds as u64);
        let finished = proc
            .tap
            .wait_for(deadline, |state| {
                if let Some((output, exit_code)) =
                    state.command.as_mut().and_then(|c| c.done.take())
                {
                    return Some((output, exit_code, false));
                }
                let exit_code = state.exited?;
                // `exit N`: the shell is gone; what it printed since the
                // command started is the output.
                let capturing = state.command.as_ref().is_some_and(|c| c.capturing);
                let output = if capturing {
                    state.scanner.text_mut().take()
                } else {
                    String::new()
                };
                Some((output, exit_code, true))
            })
            .await;

        match finished {
            Some((mut output, exit_code, shell_exited)) => {
                proc.tap.lock().command = None;
                if shell_exited {
                    // An interactive bash says `exit` as it leaves; the old
                    // non-interactive shell did not.
                    strip_exit_notice(&mut output);
                    process.take();
                    self.set_terminal(None);
                }

                // The piped shell's end marker began with a newline, so the
                // text it capped was one `\n` longer; keep omission counts
                // the same.
                output.push('\n');
                let truncated = Self::bound_output(&mut output, self.max_output_bytes);

                Ok(ShellOutput {
                    stdout: output.trim_end().to_string(),
                    exit_code,
                    truncated,
                    timed_out: false,
                })
            }
            None => {
                // Timeout - the process may be stuck. Kill it and respawn on
                // next use, but keep whatever output the command produced
                // before that instead of discarding it (AGE evidence: models
                // were retrying with hand-written `timeout N ... &`
                // wrappers because the error swallowed all prior output).
                warn!(
                    timeout = effective_timeout_seconds,
                    "Shell command timed out, killing session"
                );
                let mut output = {
                    let mut state = proc.tap.lock();
                    let capturing = state.command.as_ref().is_some_and(|c| c.capturing);
                    if capturing {
                        state.scanner.text_mut().take()
                    } else {
                        String::new()
                    }
                };
                // The shell leads its own process group, and no job control
                // keeps the command in it; bubblewrap's `--die-with-parent`
                // and PID namespace take a sandboxed one down with bwrap.
                if let Some(proc) = process.take() {
                    proc.kill();
                }
                self.set_terminal(None);

                let truncated = Self::bound_output(&mut output, self.max_output_bytes);
                let mut stdout = output.trim_end().to_string();
                if !stdout.is_empty() {
                    stdout.push('\n');
                }
                stdout.push_str(&format!(
                    "[shell_execute: command timed out after {} seconds and was killed; \
                     output above is partial. The shell session was restarted: the working \
                     directory and any exported variables are back to their defaults.]",
                    effective_timeout_seconds
                ));

                Ok(ShellOutput {
                    stdout,
                    exit_code: -1,
                    truncated,
                    timed_out: true,
                })
            }
        }
    }

    /// Set an environment variable in the shell session.
    pub async fn set_env(&self, key: &str, value: &str) -> Result<ShellOutput> {
        // Validate key contains only safe characters
        if !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(anyhow!(
                "Invalid environment variable name '{}': only alphanumeric and underscore allowed",
                key
            ));
        }

        // Use single quotes for value to prevent expansion, escaping single quotes
        let escaped_value = value.replace('\'', "'\\''");
        let command = format!("export {}='{}'", key, escaped_value);
        self.execute(&command).await
    }

    /// Change the working directory of the shell session.
    ///
    /// If a workspace directory is configured, the target path must be within it.
    pub async fn cd(&self, path: &str) -> Result<ShellOutput> {
        // If workspace is set, validate the path stays within bounds
        if let Some(ref workspace) = self.workspace_dir {
            // Resolve the path relative to current working directory in the shell
            // We do this inside the shell itself for accuracy
            let check_cmd = format!(
                "target_dir=$(cd {} 2>/dev/null && pwd) && \
                 case \"$target_dir\" in {}*) echo \"OK\";; *) echo \"DENIED\";; esac",
                shell_escape(path),
                shell_escape(workspace)
            );

            let check_result = self.execute(&check_cmd).await?;
            if check_result.stdout.trim() == "DENIED" {
                return Err(anyhow!(
                    "Cannot change directory to '{}': path is outside workspace '{}'",
                    path,
                    workspace
                ));
            }
        }

        self.execute(&format!("cd {}", shell_escape(path))).await
    }

    /// Get the current status of the shell session.
    pub async fn status(&self) -> Result<ShellStatus> {
        let process = self.process.lock().await;
        if process.is_none() {
            let uptime = SystemTime::now()
                .duration_since(self.created_at)
                .unwrap_or_default()
                .as_secs();

            return Ok(ShellStatus {
                running: false,
                cwd: self
                    .workspace_dir
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                env_vars: Vec::new(),
                pid: None,
                uptime_seconds: uptime,
            });
        }
        drop(process);

        // Get cwd and env from the running shell
        let cwd_result = self.execute("pwd").await?;
        let env_result = self.execute("env").await?;

        let cwd = cwd_result.stdout.trim().to_string();
        let env_vars: Vec<(String, String)> = env_result
            .stdout
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or("").to_string();
                // Filter out internal/noisy env vars
                if key.starts_with("__chatty") || key.starts_with("BASH_") {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();

        let pid = {
            let process = self.process.lock().await;
            process.as_ref().and_then(|p| p.terminal.pid())
        };

        let uptime = SystemTime::now()
            .duration_since(self.created_at)
            .unwrap_or_default()
            .as_secs();

        Ok(ShellStatus {
            running: true,
            cwd,
            env_vars,
            pid,
            uptime_seconds: uptime,
        })
    }

    /// Shut down the shell session, killing the bash process.
    #[allow(dead_code)]
    pub async fn shutdown(&self) {
        let mut process = self.process.lock().await;
        if let Some(proc) = process.take() {
            debug!("Shutting down shell session");
            proc.kill();
            self.set_terminal(None);
        }
    }

    /// Check if the session has a running process
    #[allow(dead_code)]
    pub async fn is_running(&self) -> bool {
        let process = self.process.lock().await;
        process.is_some()
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        // Best-effort synchronous cleanup
        if let Ok(mut process) = self.process.try_lock()
            && let Some(proc) = process.take()
        {
            debug!("Shell session dropped, killing process");
            // Dropping the last handle kills it too, but a terminal view may
            // still hold one.
            proc.kill();
        }
    }
}

/// bash's arguments: no rc files (the init loads the login profile itself,
/// bounded), interactive (it has a terminal; this makes it explicit).
fn bash_args() -> Vec<String> {
    ["--norc", "--noprofile", "-i"].map(String::from).to_vec()
}

/// Environment for the agent's terminal: pagers that print instead of
/// waiting for a key, and a width to match [`HEADLESS_COLS`]. The init sets
/// the pagers again after the login profile, which may have its own.
fn agent_env() -> HashMap<String, String> {
    let cols = HEADLESS_COLS.to_string();
    [
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("MANPAGER", "cat"),
        ("LESS", "-FRX"),
        ("TERM", "xterm-256color"),
        ("COLUMNS", cols.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Record the shell's exit code in `tap` once the PTY has been drained
/// after it: the `Wakeup` that follows `ChildExit` comes after the last read.
fn watch_exit(events: std::sync::mpsc::Receiver<TerminalEvent>, tap: Arc<Tap>) {
    let spawned = std::thread::Builder::new()
        .name("chatty-shell-exit".into())
        .spawn(move || {
            let mut exit_code = None;
            for event in events {
                match event {
                    TerminalEvent::ChildExit(status) => {
                        exit_code = Some(ShellSession::exit_code_from_status(status));
                    }
                    TerminalEvent::Wakeup if exit_code.is_some() => break,
                    _ => {}
                }
            }
            // Also reached when the terminal is dropped: a shell with no
            // exit status seen is gone all the same.
            tap.update(|state| state.exited = Some(exit_code.unwrap_or(-1)));
        });
    if let Err(e) = spawned {
        warn!(error = %e, "Could not start the shell exit watcher");
    }
}

/// Drop the `exit` line an interactive bash prints when it exits.
fn strip_exit_notice(output: &mut String) {
    let trimmed = output.trim_end();
    if let Some(rest) = trimmed.strip_suffix("exit")
        && (rest.is_empty() || rest.ends_with('\n'))
    {
        output.truncate(rest.len());
    }
}

/// Resolve the timeout to use for one `execute_with_timeout` call: the
/// per-call override when given, bounded by [`MAX_SHELL_CALL_TIMEOUT_SECONDS`]
/// so a single tool call can't block a turn indefinitely, else the session's
/// configured default.
fn resolve_call_timeout_seconds(
    default_timeout_seconds: u32,
    timeout_override: Option<u32>,
) -> u32 {
    timeout_override
        // 0 reads as "no particular limit" to a model; as a Duration it would
        // kill the command before it produced anything.
        .filter(|t| *t > 0)
        .map(|t| t.min(MAX_SHELL_CALL_TIMEOUT_SECONDS))
        .unwrap_or(default_timeout_seconds)
}

/// Escape a string for safe use in a shell command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;
