//! The model half of chatty's embedded terminal: a PTY running a shell, an
//! `alacritty_terminal` [`Term`] fed from it, and nothing else. No UI toolkit,
//! no async runtime, so it runs headless (the agent shell, Harbor runs) as
//! well as under a view.
//!
//! [`TerminalHandle::spawn`] starts the shell and alacritty's `EventLoop` on
//! its own thread. Terminal events arrive on the returned channel; drop the
//! receiver to run with no subscriber. The renderer reads the grid in place
//! through [`TerminalHandle::with_term`]; [`TerminalHandle::generation`] tells
//! it whether anything changed since the last frame.
//!
//! [`TerminalHandle::spawn_with_tap`] adds a byte-stream tap that sees every
//! byte read from the PTY before the parser does (see [`tap`]); a
//! [`MarkScanner`] fed from it finds shell-integration marks and the clean
//! text between them (see [`marks`]).
//!
//! Every terminal also keeps [`CommandRecord`]s from the OSC 133 marks a
//! shell with integration prints: [`TerminalHandle::commands`] and
//! [`Region::LastCommand`]. bash and zsh started with default arguments get
//! that integration injected (see [`TerminalConfig::shell_integration`]).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{self, EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Osc52};
use alacritty_terminal::tty;

mod commands;
#[cfg(unix)]
mod integration;
pub mod marks;
mod snapshot;
pub mod tap;

pub use alacritty_terminal;
pub use commands::{CommandRecord, LAST_COMMAND_OUTPUT_BYTES, LastCommand, MAX_RECORDS};
pub use marks::{CHATTY_OSC, CleanText, Mark, MarkScanner};
pub use snapshot::{Region, TerminalText};
pub use tap::ByteTap;

/// Events forwarded from the terminal to whoever subscribes.
///
/// This is alacritty's own event type. `PtyWrite` (replies to terminal
/// queries such as cursor-position reports) and `TextAreaSizeRequest` are
/// answered inside this crate and never forwarded, so a headless terminal
/// with no subscriber still answers the programs running in it.
pub type TerminalEvent = Event;

/// The terminal model the renderer reads.
pub type TerminalModel = Term<EventProxy>;

/// How to start a terminal.
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Program to run. `None` runs the platform default shell: `$SHELL`
    /// (fallback `/bin/bash`) on Unix, `pwsh` if on `PATH` else
    /// `powershell` on Windows.
    pub shell: Option<String>,
    /// Arguments. With `shell: None` and no arguments, the default shell
    /// starts as a login + interactive shell (`-l -i`) on Unix.
    pub args: Vec<String>,
    /// Working directory; `None` inherits this process's.
    pub cwd: Option<PathBuf>,
    /// Extra environment. `TERM=xterm-256color` and `COLORTERM=truecolor`
    /// are set unless given here.
    pub env: HashMap<String, String>,
    /// Grid size as `(cols, rows)`.
    pub size: (u16, u16),
    /// Cell size in pixels as `(width, height)`, reported to the child via
    /// the window size (`TIOCGWINSZ` pixel fields).
    pub cell_px: (u16, u16),
    /// Lines of scrollback kept above the screen.
    pub scrollback: usize,
    /// Load chatty's OSC 133 shell integration into bash and zsh (Unix),
    /// so [`TerminalHandle::commands`] sees each command. Applies only when
    /// `args` is empty (the default arguments); the user's own startup
    /// files still load first and are never modified. Default `true`.
    pub shell_integration: bool,
}

/// Default for [`TerminalConfig::scrollback`], alacritty's own default.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
            size: (80, 24),
            cell_px: (8, 16),
            scrollback: DEFAULT_SCROLLBACK,
            shell_integration: true,
        }
    }
}

/// The `EventListener` given to alacritty. Answers PTY replies itself,
/// bumps the generation counter, and forwards the rest.
pub struct EventProxy {
    events: Sender<TerminalEvent>,
    shared: Arc<Shared>,
}

struct Shared {
    /// Set once the event loop exists (the `Term` is built before it).
    loop_tx: OnceLock<EventLoopSender>,
    generation: AtomicU64,
    exited: AtomicBool,
    size: Mutex<WindowSize>,
}

impl Shared {
    fn send(&self, msg: Msg) -> io::Result<()> {
        let tx = self
            .loop_tx
            .get()
            .ok_or_else(|| io::Error::other("terminal event loop not started"))?;
        tx.send(msg).map_err(io::Error::other)
    }

    fn window_size(&self) -> WindowSize {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                if let Err(e) = self.shared.send(Msg::Input(Cow::Owned(text.into_bytes()))) {
                    tracing::warn!("terminal: dropping PTY reply: {e}");
                }
                return;
            }
            Event::TextAreaSizeRequest(format) => {
                let reply = format(self.shared.window_size());
                if let Err(e) = self.shared.send(Msg::Input(Cow::Owned(reply.into_bytes()))) {
                    tracing::warn!("terminal: dropping size reply: {e}");
                }
                return;
            }
            Event::Wakeup => {
                self.shared.generation.fetch_add(1, Ordering::Release);
            }
            Event::ChildExit(_) => {
                self.shared.exited.store(true, Ordering::Release);
            }
            _ => {}
        }
        // No subscriber (headless) is fine.
        let _ = self.events.send(event);
    }
}

/// A running terminal: the shell's PTY and the `Term` it feeds.
///
/// Dropping the handle kills the child and joins the PTY thread, so no shell
/// outlives it.
pub struct TerminalHandle {
    term: Arc<FairMutex<TerminalModel>>,
    shared: Arc<Shared>,
    commands: Arc<Mutex<commands::CommandTracker>>,
    pid: Option<u32>,
    /// A duplicate of the PTY master, kept to ask which process group has
    /// the terminal's foreground ([`Self::has_foreground_job`]).
    #[cfg(unix)]
    master: Option<std::fs::File>,
    /// The PTY thread. It hands back the event loop, and with it the PTY,
    /// when it stops; dropping that runs the PTY's own cleanup (hang up,
    /// reap the child).
    thread: Option<JoinHandle<(PtyLoop, event_loop::State)>>,
}

type PtyLoop = EventLoop<tap::Tapped<tty::Pty>, EventProxy>;

impl TerminalHandle {
    /// Start a terminal. Returns the handle and the event channel; drop the
    /// receiver to run headless.
    pub fn spawn(config: TerminalConfig) -> io::Result<(Self, Receiver<TerminalEvent>)> {
        Self::spawn_inner(config, None)
    }

    /// Like [`spawn`](Self::spawn), with `tap` seeing every byte read from
    /// the PTY, in order, before the terminal parser does. The tap runs on
    /// the PTY thread.
    pub fn spawn_with_tap(
        config: TerminalConfig,
        tap: impl FnMut(&[u8]) + Send + 'static,
    ) -> io::Result<(Self, Receiver<TerminalEvent>)> {
        Self::spawn_inner(config, Some(Box::new(tap)))
    }

    fn spawn_inner(
        config: TerminalConfig,
        mut tap: Option<ByteTap>,
    ) -> io::Result<(Self, Receiver<TerminalEvent>)> {
        let (cols, rows) = config.size;
        let (cell_width, cell_height) = config.cell_px;
        let scrollback = config.scrollback;
        let window_size = WindowSize {
            num_cols: cols.max(1),
            num_lines: rows.max(1),
            cell_width,
            cell_height,
        };

        let pty = tty::new(&pty_options(config), window_size, 0)?;
        let pid = child_pid(&pty);
        #[cfg(unix)]
        let master = pty
            .file()
            .try_clone()
            .inspect_err(|e| tracing::warn!("terminal: cannot duplicate the PTY master: {e}"))
            .ok();

        let shared = Arc::new(Shared {
            loop_tx: OnceLock::new(),
            generation: AtomicU64::new(0),
            exited: AtomicBool::new(false),
            size: Mutex::new(window_size),
        });
        let (events_tx, events_rx) = mpsc::channel();
        let proxy = || EventProxy {
            events: events_tx.clone(),
            shared: Arc::clone(&shared),
        };

        // OSC 52: a program may set the clipboard (forwarded as
        // `ClipboardStore`) but never read it; `OnlyCopy` drops the
        // read request inside `Term`, so it is never answered.
        let term_config = TermConfig {
            osc52: Osc52::OnlyCopy,
            scrolling_history: scrollback,
            ..TermConfig::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            term_config,
            &GridSize::from(window_size),
            proxy(),
        )));
        let commands = Arc::new(Mutex::new(commands::CommandTracker::new(&GridSize::from(
            window_size,
        ))));
        let tap: ByteTap = Box::new({
            let commands = Arc::clone(&commands);
            move |bytes| {
                lock(&commands).feed(bytes);
                if let Some(tap) = tap.as_mut() {
                    tap(bytes);
                }
            }
        });
        let event_loop = EventLoop::new(
            Arc::clone(&term),
            proxy(),
            tap::Tapped::new(pty, Some(tap)),
            true,
            false,
        )?;
        let _ = shared.loop_tx.set(event_loop.channel());
        let thread = event_loop.spawn();

        Ok((
            Self {
                term,
                shared,
                commands,
                pid,
                #[cfg(unix)]
                master,
                thread: Some(thread),
            },
            events_rx,
        ))
    }

    /// Send input bytes to the child.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            // alacritty's loop stalls on an empty write.
            return Ok(());
        }
        self.shared.send(Msg::Input(Cow::Owned(bytes.to_vec())))
    }

    /// Resize the grid and the PTY (the child gets `SIGWINCH` on Unix).
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let size = {
            let mut size = self.shared.size.lock().unwrap_or_else(|e| e.into_inner());
            size.num_cols = cols.max(1);
            size.num_lines = rows.max(1);
            *size
        };
        {
            // The lease keeps the PTY thread out of its read loop, so the
            // tracker and the terminal have seen the same bytes.
            let _lease = self.term.lease();
            let mut term = self.term.lock_unfair();
            lock(&self.commands).resize(&mut term, GridSize::from(size));
        }
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.shared.send(Msg::Resize(size))
    }

    /// Kill the child. On Unix the whole process group gets `SIGKILL`, and
    /// [`TerminalEvent::ChildExit`] follows once it is reaped. On Windows the
    /// PTY is shut down, which closes the pseudoconsole and ends the child;
    /// no `ChildExit` is sent there.
    pub fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid
            && !self.has_exited()
        {
            // The shell ran `setsid()`, so its pid is its process-group id.
            // Guarded by `has_exited` so a reaped (and maybe reused) pid is
            // not signalled; the window between reap and flag is the event
            // loop's own few instructions.
            let pgid = nix::unistd::Pid::from_raw(pid as i32);
            if let Err(e) = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL)
                && e != nix::errno::Errno::ESRCH
            {
                tracing::warn!("terminal: killpg({pid}) failed: {e}");
            }
        }
        #[cfg(windows)]
        let _ = self.shared.send(Msg::Shutdown);
    }

    /// Whether the child has exited (a `ChildExit` was seen).
    pub fn has_exited(&self) -> bool {
        self.shared.exited.load(Ordering::Acquire)
    }

    /// Whether a program other than the shell holds the terminal's
    /// foreground (e.g. `vim`, `cargo build`): the PTY's foreground process
    /// group is not the shell's own. `None` where that cannot be told
    /// (Windows, or the query failed); `Some(false)` once the shell exited.
    pub fn has_foreground_job(&self) -> Option<bool> {
        if self.has_exited() {
            return Some(false);
        }
        #[cfg(unix)]
        {
            let (master, pid) = (self.master.as_ref()?, self.pid?);
            // The shell ran `setsid()`, so its pid is its process-group id.
            let foreground = nix::unistd::tcgetpgrp(master).ok()?;
            Some(foreground.as_raw() != pid as i32)
        }
        #[cfg(not(unix))]
        None
    }

    /// Whether the terminal is at a plain text password prompt: the PTY's
    /// `ECHO` flag is off while the line discipline is canonical (`ICANON`),
    /// which is what `sudo`, `ssh`, `su`, `passwd`, `getpass(3)`, `read -s`
    /// and gpg's `--pinentry-mode loopback` prompt set while a password or
    /// passphrase is typed. `ECHO` alone is not enough: line editors (bash's
    /// readline, zsh's zle) and full-screen programs turn it off too, but
    /// they also leave canonical mode, and they draw what is typed
    /// themselves.
    ///
    /// Not caught: full-screen (curses) passphrase boxes such as
    /// pinentry-curses, which run in raw mode like vim or less and cannot
    /// be told apart from them by termios, and GUI pinentries outside the
    /// terminal. In both the passphrase itself is never on screen; only the
    /// surrounding screen is, as at any other moment.
    ///
    /// `None` where that cannot be told (Windows: ConPTY exposes no termios),
    /// after exit, or on error.
    pub fn input_hidden(&self) -> Option<bool> {
        if self.has_exited() {
            return None;
        }
        #[cfg(unix)]
        {
            use nix::sys::termios::LocalFlags;
            let flags = nix::sys::termios::tcgetattr(self.master.as_ref()?)
                .ok()?
                .local_flags;
            Some(is_hidden_input(
                flags.contains(LocalFlags::ECHO),
                flags.contains(LocalFlags::ICANON),
            ))
        }
        #[cfg(not(unix))]
        None
    }

    /// Name of the program holding the terminal's foreground (`vim`,
    /// `cargo`, or the shell itself at its prompt), for a tab title. Linux
    /// only (read from `/proc`); `None` elsewhere, after exit, or on error.
    pub fn foreground_process_name(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            if self.has_exited() {
                return None;
            }
            let pgid = nix::unistd::tcgetpgrp(self.master.as_ref()?).ok()?;
            // A process group's id is its leader's pid.
            let comm = std::fs::read_to_string(format!("/proc/{pgid}/comm")).ok()?;
            let name = comm.trim();
            (!name.is_empty()).then(|| name.to_string())
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// The shell's current working directory, for a tab title. Linux only
    /// (read from `/proc`); `None` elsewhere, after exit, or on error.
    pub fn current_dir(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            if self.has_exited() {
                return None;
            }
            std::fs::read_link(format!("/proc/{}/cwd", self.pid?)).ok()
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// The child's process id, if the platform reports one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Read the terminal under its lock, without copying the grid.
    pub fn with_term<R>(&self, f: impl FnOnce(&TerminalModel) -> R) -> R {
        f(&self.term.lock())
    }

    /// Change the terminal under its lock: scroll the viewport, set the
    /// selection. Bumps [`generation`](Self::generation) so the view repaints.
    pub fn with_term_mut<R>(&self, f: impl FnOnce(&mut TerminalModel) -> R) -> R {
        let result = f(&mut self.term.lock());
        self.shared.generation.fetch_add(1, Ordering::Release);
        result
    }

    /// Plain text of `region`.
    pub fn snapshot(&self, region: Region) -> TerminalText {
        if region == Region::LastCommand {
            return self.with_commands(|term, commands| {
                let (last, output) = commands.last_command(term);
                snapshot::frame(term, output, Some(last))
            });
        }
        snapshot::snapshot(&self.term.lock(), region)
    }

    /// The commands run at an integrated shell's prompt, oldest first (the
    /// last [`MAX_RECORDS`]). Empty for a shell without integration.
    pub fn commands(&self) -> Vec<CommandRecord> {
        self.with_commands(|term, commands| commands.records(term))
    }

    /// Run `f` on the terminal and the command tracker once both have
    /// processed the same bytes: under the lease the PTY thread is outside
    /// its read loop, and every byte the tap saw has been parsed.
    fn with_commands<R>(
        &self,
        f: impl FnOnce(&TerminalModel, &mut commands::CommandTracker) -> R,
    ) -> R {
        let _lease = self.term.lease();
        let term = self.term.lock_unfair();
        f(&term, &mut lock(&self.commands))
    }

    /// Counter bumped whenever the terminal content or size changes. A view
    /// that remembers the last value it painted can skip unchanged frames.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        self.kill();
        let _ = self.shared.send(Msg::Shutdown);
        if let Some(thread) = self.thread.take() {
            // Dropping the returned loop drops the PTY.
            drop(thread.join());
        }
    }
}

/// A password prompt: canonical line input that is not echoed.
#[cfg(unix)]
fn is_hidden_input(echo: bool, canonical: bool) -> bool {
    !echo && canonical
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// `Dimensions` for a bare grid size.
#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl From<WindowSize> for GridSize {
    fn from(size: WindowSize) -> Self {
        Self {
            cols: size.num_cols as usize,
            rows: size.num_lines as usize,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn pty_options(config: TerminalConfig) -> tty::Options {
    let login = config.shell.is_none();
    let default_args = config.args.is_empty();
    let mut env = config.env;
    #[cfg(not(unix))]
    let _ = (login, default_args, config.shell_integration);

    let (program, args) = match config.shell {
        Some(program) => (program, config.args),
        None => {
            let args = if config.args.is_empty() {
                default_shell_args()
            } else {
                config.args
            };
            (default_shell(), args)
        }
    };

    #[cfg(unix)]
    let args = match config.shell_integration && default_args {
        true => integration::inject(&program, login, &mut env).unwrap_or(args),
        false => args,
    };

    env.entry("TERM".into())
        .or_insert_with(|| "xterm-256color".into());
    env.entry("COLORTERM".into())
        .or_insert_with(|| "truecolor".into());

    tty::Options {
        shell: Some(tty::Shell::new(program, args)),
        working_directory: config.cwd,
        drain_on_exit: true,
        env,
        #[cfg(windows)]
        escape_args: true,
    }
}

#[cfg(unix)]
fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/bash".into())
}

#[cfg(unix)]
fn default_shell_args() -> Vec<String> {
    vec!["-l".into(), "-i".into()]
}

#[cfg(windows)]
fn default_shell() -> String {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("pwsh.exe").is_file()));
    if on_path { "pwsh" } else { "powershell" }.into()
}

#[cfg(windows)]
fn default_shell_args() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
fn child_pid(pty: &tty::Pty) -> Option<u32> {
    Some(pty.child().id())
}

#[cfg(windows)]
fn child_pid(pty: &tty::Pty) -> Option<u32> {
    pty.child_watcher().pid().map(|pid| pid.get())
}
