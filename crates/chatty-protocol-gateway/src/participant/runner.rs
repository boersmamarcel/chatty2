//! Spawning a chatty child and exposing it as an A2A participant.
//!
//! ADR-0011's C2. The broker publishes one *virtual* agent — `local-agent` by
//! default — that is not a connected process but a factory: a task addressed
//! to it spawns a child, waits for that child to register over the
//! participant socket, and routes the task to it. To the caller it is an A2A
//! agent like any other, which is the whole point: `invoke_agent` replaces
//! `sub_agent` without the parent learning a second fan-out path.
//!
//! # Lifetime
//!
//! One child per task. The child is capable of serving tasks until its socket
//! closes, but the runner's policy is one-shot, which keeps the process
//! lifecycle identical to the `sub_agent` it replaces — that equivalence is
//! what makes ADR-0011's second kill criterion (AGE-302) a fair comparison
//! rather than a comparison of process-reuse strategies. Making a worker
//! persistent is a change to this file and nothing else.
//!
//! # Where a worker runs
//!
//! ADR-0012 gives each worker its own copy of the workspace, which on the
//! desktop is a `git worktree`. Making one is a git operation and git lives
//! in `chatty-core`, so this crate does not decide: the embedder supplies a
//! [`WorkspaceFactory`], and the broker only spawns in whatever directory it
//! is handed. Without a factory the child inherits the broker's own
//! directory, exactly as `sub_agent` did before AGE-314.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use super::protocol::{ParticipantCard, ParticipantSkill};
use super::registry::{ParticipantRegistry, TaskStream};
use super::virtual_agent::{VirtualAgent, WorkerFuture, WorkerHandle};

/// A worker's directory, and what to do with it once the worker is gone.
pub struct WorkerWorkspace {
    /// The child's working directory, and the root its tools are confined to.
    pub cwd: PathBuf,
    /// Run after the worker exits, with whether its task succeeded. This is
    /// where ADR-0016's turn-commit barrier goes: a worker's output has to be
    /// durable before anything may remove the tree it lives in.
    ///
    /// Synchronous because it runs from `Drop`, which cannot await. An
    /// implementation that needs to await — committing to a branch does —
    /// spawns; the work outlives the worker either way, and the tree it
    /// operates on is nobody else's.
    pub on_exit: Box<dyn FnOnce(bool) + Send>,
}

impl std::fmt::Debug for WorkerWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerWorkspace")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// What [`WorkspaceFactory`] returns. Boxed by hand rather than through
/// `futures`, which this crate does not depend on.
pub type WorkspaceFuture = Pin<Box<dyn Future<Output = Result<Option<WorkerWorkspace>>> + Send>>;

/// Makes a directory for one worker, named by its participant name.
///
/// Async because making one is a `git worktree add` — a subprocess, not a
/// `mkdir`. `Ok(None)` means "no isolation available" and the child runs
/// where the broker does; that is a real fallback, not an error, because a
/// workspace that is not a git repository has no worktree to give (AGE-314's
/// open question).
pub type WorkspaceFactory = Arc<dyn Fn(String) -> WorkspaceFuture + Send + Sync + 'static>;

/// How long a child gets to connect and register before the task fails.
///
/// Generous, because it covers process start-up on a cold page cache; a real
/// hang is caught by the caller's own HTTP timeout, not by shaving this.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the runner checks whether the child has registered yet.
const REGISTRATION_POLL: Duration = Duration::from_millis(5);

/// How much of a worker's stderr to keep for an error message.
///
/// A tail, not a head: when a worker dies, the last thing it said is the
/// reason. It is drained *continuously* rather than read on failure — a piped
/// stream nobody reads fills its buffer and blocks the writer, which for a
/// chatty child that logs every tool call is a deadlock, not a slow path.
const STDERR_TAIL_BYTES: usize = 8 * 1024;

/// Spawns chatty children and exposes them under one agent name.
pub struct LocalRunner {
    agent_name: String,
    description: String,
    executable: PathBuf,
    socket: PathBuf,
    /// Extra arguments every child gets — the model id, `--auto-approve`.
    args: Vec<String>,
    workspace: Option<WorkspaceFactory>,
    registry: ParticipantRegistry,
    seq: AtomicU64,
    registration_timeout: Duration,
}

impl LocalRunner {
    /// `executable` is the chatty binary to spawn; `socket` is the path its
    /// children register on, which must be the one the gateway is serving.
    pub fn new(
        executable: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
        registry: ParticipantRegistry,
    ) -> Self {
        Self {
            agent_name: "local-agent".to_string(),
            description: "A chatty agent in its own process, with its own \
                          workspace and tool set. Delegate a self-contained \
                          task to it and it works autonomously and reports back."
                .to_string(),
            executable: executable.into(),
            socket: socket.into(),
            args: Vec::new(),
            workspace: None,
            registry,
            seq: AtomicU64::new(0),
            registration_timeout: REGISTRATION_TIMEOUT,
        }
    }

    /// Rename the virtual agent callers address.
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    /// Arguments every child is spawned with, e.g. `--model`, `--auto-approve`.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Give each worker its own directory (ADR-0012).
    pub fn with_workspace_factory(mut self, factory: WorkspaceFactory) -> Self {
        self.workspace = Some(factory);
        self
    }

    /// How long a child gets to connect and register before the task fails.
    /// Defaults to [`REGISTRATION_TIMEOUT`].
    pub fn with_registration_timeout(mut self, timeout: Duration) -> Self {
        self.registration_timeout = timeout;
        self
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// The registry its workers register in — the same one the gateway
    /// serves, so a spawned worker is reachable by name like any other.
    pub fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    /// The card served at `/a2a/{agent_name}/.well-known/agent.json`.
    ///
    /// It describes the *kind* of worker the runner spawns, since no
    /// particular worker exists until a task arrives.
    pub fn agent_card(&self) -> ParticipantCard {
        ParticipantCard {
            name: self.agent_name.clone(),
            display_name: Some("Local agent".to_string()),
            description: self.description.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            skills: vec![ParticipantSkill {
                name: "delegate".to_string(),
                description: "Run a self-contained task in a separate process \
                              and report the result."
                    .to_string(),
                examples: vec![
                    "Summarise every TODO in src/ and list the files they are in".to_string(),
                ],
            }],
        }
    }

    /// Spawn a worker and hand it `prompt`.
    ///
    /// The returned [`Worker`] owns the child process: dropping it reaps the
    /// child and runs the workspace's `on_exit`, so a caller that hangs up
    /// mid-task does not leak a process or an uncommitted worktree. It is
    /// returned alongside the task's update stream rather than owning it, so
    /// the caller can read updates while still holding the process handle.
    pub async fn run_task(&self, prompt: String) -> Result<(Worker, TaskStream)> {
        let name = format!(
            "{}-{}",
            self.agent_name,
            self.seq.fetch_add(1, Ordering::Relaxed)
        );

        let workspace = match self.workspace.as_ref() {
            Some(factory) => factory(name.clone())
                .await
                .with_context(|| format!("failed to prepare a workspace for worker '{name}'"))?,
            None => None,
        };

        let mut child = self.spawn(&name, workspace.as_ref())?;
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let stderr_drain = drain_stderr(&mut child, stderr_tail.clone());
        let mut worker = Worker {
            name: name.clone(),
            child: Some(child),
            workspace,
            registry: self.registry.clone(),
            task_id: None,
            succeeded: false,
            stderr_tail,
            stderr_drain,
        };

        self.await_registration(&mut worker).await?;

        let (task_id, updates) = self
            .registry
            .submit_task(&name, prompt)
            .ok_or_else(|| anyhow!("worker '{name}' disconnected before it could be given work"))?;
        worker.task_id = Some(task_id.clone());

        info!(worker = %name, task = %task_id, "Delegated a task to a local worker");
        Ok((worker, updates))
    }

    fn spawn(&self, name: &str, workspace: Option<&WorkerWorkspace>) -> Result<Child> {
        let mut cmd = Command::new(&self.executable);
        cmd.args(&self.args)
            .arg("--participant-socket")
            .arg(&self.socket)
            .arg("--participant-name")
            .arg(name)
            .stdin(std::process::Stdio::null())
            // The answer comes back over the socket, so the child's stdout is
            // redundant; discarded rather than piped, so there is one less
            // stream that could fill and block it.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(workspace) = workspace {
            // Both: `current_dir` for anything that reads the process CWD, and
            // `--workspace` because chatty-tui's tool root comes from the
            // settings file it shares with its parent unless overridden
            // (AGE-314).
            cmd.current_dir(&workspace.cwd)
                .arg("--workspace")
                .arg(&workspace.cwd);
        }

        debug!(worker = %name, exe = ?self.executable, "Spawning a local worker");
        cmd.spawn()
            .with_context(|| format!("failed to spawn {}", self.executable.display()))
    }

    /// Wait for the child to register, or fail with why it did not.
    async fn await_registration(&self, worker: &mut Worker) -> Result<()> {
        let deadline = tokio::time::Instant::now() + self.registration_timeout;

        loop {
            if self.registry.is_registered(&worker.name) {
                return Ok(());
            }

            // A child that died is a better error than a timeout: its exit
            // status and stderr say what actually went wrong.
            if let Some(child) = worker.child.as_mut()
                && let Some(status) = child.try_wait()?
            {
                let stderr = worker.stderr_tail().await;
                bail!(
                    "worker '{}' exited before registering ({status}){}",
                    worker.name,
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    }
                );
            }

            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "worker '{}' did not register within {:?}",
                    worker.name,
                    self.registration_timeout
                );
            }
            tokio::time::sleep(REGISTRATION_POLL).await;
        }
    }
}

/// The broker's view of the runner: a card, and a worker per task.
///
/// The inherent methods stay because they return the concrete [`Worker`],
/// which the A/A benchmark and the tests drive directly; the trait is what
/// the gateway holds, so a hosted runner that leases a microVM (AGE-307) can
/// take the same slot without the handlers learning a second shape.
impl VirtualAgent for LocalRunner {
    fn agent_name(&self) -> &str {
        LocalRunner::agent_name(self)
    }

    fn agent_card(&self) -> ParticipantCard {
        LocalRunner::agent_card(self)
    }

    fn registry(&self) -> &ParticipantRegistry {
        LocalRunner::registry(self)
    }

    fn run_task(&self, prompt: String) -> WorkerFuture<'_> {
        Box::pin(async move {
            let (worker, updates) = LocalRunner::run_task(self, prompt).await?;
            Ok((Box::new(worker) as Box<dyn WorkerHandle>, updates))
        })
    }
}

/// A spawned worker and its task.
///
/// Dropping it kills and reaps the child and runs the workspace's `on_exit`.
/// That happens on every path — the task completing, the caller hanging up,
/// the broker shutting down — because the alternative is an orphaned agent
/// process holding a worktree.
pub struct Worker {
    name: String,
    child: Option<Child>,
    workspace: Option<WorkerWorkspace>,
    registry: ParticipantRegistry,
    task_id: Option<String>,
    succeeded: bool,
    /// The tail of the child's stderr, kept for error messages.
    stderr_tail: Arc<Mutex<String>>,
    stderr_drain: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("name", &self.name)
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

impl Worker {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    /// Record how the task ended, for the workspace's `on_exit`.
    pub fn set_succeeded(&mut self, succeeded: bool) {
        self.succeeded = succeeded;
    }

    /// The last thing the child said, for an error message.
    ///
    /// Awaits the drain task first: the child has exited by the time this is
    /// called, so its stderr is closed and the task is about to finish — and
    /// without the await, the report would race the reason.
    async fn stderr_tail(&mut self) -> String {
        if let Some(drain) = self.stderr_drain.take() {
            let _ = drain.await;
        }
        let tail = self
            .stderr_tail
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        let tail = tail.trim();
        match tail.char_indices().nth_back(499) {
            Some((at, _)) => tail[at..].to_string(),
            None => tail.to_string(),
        }
    }
}

impl WorkerHandle for Worker {
    fn name(&self) -> &str {
        Worker::name(self)
    }

    fn task_id(&self) -> Option<&str> {
        Worker::task_id(self)
    }

    /// A local worker has no second line item: the tokens in `metadata` are
    /// the whole cost of a child process, and the ledger that would record
    /// them is the hosted one (AGE-307).
    fn finish(&mut self, succeeded: bool, _metadata: Option<&Value>) {
        Worker::set_succeeded(self, succeeded)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(task_id) = self.task_id.take() {
            self.registry.cancel_task(&self.name, &task_id);
        }
        // `kill_on_drop` reaps the child; the registry drops the participant
        // when the socket closes behind it.
        if self.child.take().is_some() {
            debug!(worker = %self.name, "Reaping a local worker");
        }
        if let Some(workspace) = self.workspace.take() {
            let succeeded = self.succeeded;
            (workspace.on_exit)(succeeded);
        }
        warn_if_still_registered(&self.registry, &self.name);
    }
}

fn warn_if_still_registered(registry: &ParticipantRegistry, name: &str) {
    if registry.is_registered(name) {
        debug!(
            worker = %name,
            "Worker still registered at reap; its socket close will deregister it"
        );
    }
}

/// Keep reading the child's stderr so it can never block on a full pipe,
/// retaining only the tail.
fn drain_stderr(child: &mut Child, tail: Arc<Mutex<String>>) -> Option<JoinHandle<()>> {
    let stderr = child.stderr.take()?;
    Some(tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut tail) = tail.lock() else { return };
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > STDERR_TAIL_BYTES {
                let cut = tail.len() - STDERR_TAIL_BYTES;
                let cut = (cut..tail.len())
                    .find(|i| tail.is_char_boundary(*i))
                    .unwrap_or(tail.len());
                tail.drain(..cut);
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::protocol::BrokerFrame;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::mpsc;

    /// A "chatty binary" that ignores the flags the runner appends: `sh -c`
    /// turns anything after the command into positional parameters, so the
    /// script runs unchanged.
    fn fake_binary(script: &str) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), script.to_string()],
        )
    }

    fn runner(registry: ParticipantRegistry, script: &str) -> LocalRunner {
        let (exe, args) = fake_binary(script);
        LocalRunner::new(exe, "/nonexistent/participants.sock", registry)
            .with_args(args)
            .with_registration_timeout(Duration::from_secs(5))
    }

    /// Register `name` once it appears, standing in for a child that
    /// connected over the socket.
    fn register_when_asked(
        registry: ParticipantRegistry,
        name: &str,
    ) -> mpsc::UnboundedReceiver<BrokerFrame> {
        let (tx, rx) = mpsc::unbounded_channel();
        registry
            .register(
                ParticipantCard {
                    name: name.to_string(),
                    ..Default::default()
                },
                tx,
            )
            .expect("the stand-in worker registers");
        rx
    }

    #[tokio::test]
    async fn a_registered_worker_is_handed_the_task() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry.clone(), "sleep 30");

        // The name is allocated before the spawn, so a stand-in can claim it.
        let mut outbound = register_when_asked(registry.clone(), "local-agent-0");

        let (worker, _updates) = runner
            .run_task("summarise foo.rs".into())
            .await
            .expect("the worker registered, so the task is delegated");

        assert_eq!(worker.name(), "local-agent-0");
        assert!(worker.task_id().is_some());
        let BrokerFrame::Task { text, task_id } = outbound.recv().await.unwrap() else {
            panic!("the worker is sent a task frame");
        };
        assert_eq!(text, "summarise foo.rs");
        assert_eq!(Some(task_id.as_str()), worker.task_id());
    }

    #[tokio::test]
    async fn a_worker_that_dies_before_registering_reports_why() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry, "echo 'no model configured' >&2; exit 3");

        let error = runner
            .run_task("anything".into())
            .await
            .expect_err("a child that exits cannot take a task");
        let text = format!("{error:#}");

        assert!(text.contains("exited before registering"), "{text}");
        assert!(
            text.contains("no model configured"),
            "the child's own stderr is the useful part: {text}"
        );
    }

    #[tokio::test]
    async fn a_worker_that_never_registers_times_out_rather_than_hanging() {
        let registry = ParticipantRegistry::new();
        let runner =
            runner(registry, "sleep 30").with_registration_timeout(Duration::from_millis(150));

        let error = runner
            .run_task("anything".into())
            .await
            .expect_err("a child that never registers fails the task");
        assert!(
            format!("{error:#}").contains("did not register"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn each_worker_gets_a_workspace_and_gives_it_back() {
        let registry = ParticipantRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        let released = Arc::new(AtomicBool::new(false));

        let runner = runner(registry.clone(), "sleep 30").with_workspace_factory({
            let cwd = dir.path().to_path_buf();
            let released = released.clone();
            Arc::new(move |worker: String| {
                assert_eq!(worker, "local-agent-0", "the factory is told who it is for");
                let cwd = cwd.clone();
                let released = released.clone();
                Box::pin(async move {
                    Ok(Some(WorkerWorkspace {
                        cwd,
                        on_exit: Box::new(move |_| {
                            released.store(true, Ordering::Relaxed);
                        }),
                    }))
                })
            })
        });
        let _outbound = register_when_asked(registry, "local-agent-0");

        let (worker, _updates) = runner.run_task("work".into()).await.unwrap();
        assert!(
            !released.load(Ordering::Relaxed),
            "the workspace is held while the worker is alive"
        );

        drop(worker);
        assert!(
            released.load(Ordering::Relaxed),
            "reaping a worker releases its workspace, so nothing is left uncommitted"
        );
    }

    #[tokio::test]
    async fn a_failing_workspace_fails_the_task_rather_than_running_unisolated() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry, "sleep 30").with_workspace_factory(Arc::new(|_| {
            Box::pin(async { Err(anyhow!("the worktree could not be created")) })
        }));

        let error = runner
            .run_task("work".into())
            .await
            .expect_err("ADR-0012 isolation that was asked for and failed is not silently skipped");
        assert!(format!("{error:#}").contains("worktree"), "{error:#}");
    }

    #[tokio::test]
    async fn dropping_a_worker_cancels_its_task() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry.clone(), "sleep 30");
        let mut outbound = register_when_asked(registry.clone(), "local-agent-0");

        let (worker, mut updates) = runner.run_task("work".into()).await.unwrap();
        let _ = outbound.recv().await;
        assert_eq!(registry.open_task_count("local-agent-0"), 1);

        drop(worker);

        assert!(matches!(
            outbound.recv().await,
            Some(BrokerFrame::Cancel { .. })
        ));
        assert_eq!(registry.open_task_count("local-agent-0"), 0);
        assert!(updates.recv().await.is_none(), "the caller's stream ends");
    }

    #[test]
    fn the_agent_card_describes_the_worker_it_would_spawn() {
        let runner = LocalRunner::new("/bin/sh", "/tmp/x.sock", ParticipantRegistry::new());
        let card = runner.agent_card();
        assert_eq!(card.name, "local-agent");
        assert_eq!(card.skills[0].name, "delegate");
        assert!(!card.description.is_empty());
    }
}
