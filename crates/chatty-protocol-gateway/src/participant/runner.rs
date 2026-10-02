//! Spawning a chatty child and exposing it as an A2A participant.
//!
//! ADR-0011's C2. The broker publishes *virtual* agents — `local-agent` by
//! default; a named team of them under C10 — each of which is not a
//! connected process but a factory: a task addressed to it admits a node,
//! makes a socket pair for it, spawns a child holding one end, waits for
//! that child's `hello`, and routes the task to it (ADR-0020). To the caller
//! it is an A2A agent like any other, which is the whole point: one fan-out
//! path for the parent, whoever ends up serving the task. Two runners differ only in name and argv (`--model`,
//! `--disable`), so one binary serves every role.
//!
//! # Lifetime
//!
//! One child per task. The child is capable of serving tasks until its
//! socket closes, but the runner's policy is one-shot: a task gets a
//! process, and the process dies with it. Making a worker persistent is a
//! change to this file and nothing else.
//!
//! # Where a worker runs
//!
//! ADR-0012 gives each worker its own copy of the workspace, which on the
//! desktop is a `git worktree`. Making one is a git operation and git lives
//! in `chatty-core`, so this crate does not decide: the embedder supplies a
//! [`WorkspaceFactory`], and the broker only spawns in whatever directory it
//! is handed. Without a factory the child inherits the broker's own
//! directory.

use std::future::Future;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chatty_fabric::{EndpointBudget, HandoffContract, RunPermit, SpawnContext};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{debug, info};

use super::listener::{LocalConnection, open_connection};
use super::protocol::{DelegatedTask, ParticipantCard, ParticipantSkill};
use super::registry::{ParticipantRegistry, TaskStream};
use super::virtual_agent::{EvidenceFuture, VirtualAgent, WorkerFuture, WorkerHandle};

/// A worker's directory, and what to do with it once the worker is gone.
pub struct WorkerWorkspace {
    /// The child's working directory, and the root its tools are confined to.
    pub cwd: PathBuf,
    /// The branch its output is committed to, when the directory is a
    /// branch of its own: what the worker's own calls spawn from (BI-5).
    pub branch: Option<String>,
    /// Collects the evidence envelope once the worker's task ends: it
    /// commits the tree and reads back the branch, the diff stat, the
    /// commit count and the team's verification result (AGE-406). Awaited
    /// by the handler before the terminal event, since the caller stops
    /// reading at one.
    ///
    /// `None` when the workspace has nothing to report — and the collector
    /// itself may still answer `None`, which is what an empty branch does.
    pub evidence: Option<EvidenceFactory>,
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
            .field("evidence", &self.evidence.is_some())
            .finish_non_exhaustive()
    }
}

/// Makes the evidence envelope for one finished worker. `FnOnce` because
/// collecting it commits the tree, which must happen exactly once.
pub type EvidenceFactory = Box<dyn FnOnce() -> EvidenceFuture + Send>;

/// What [`WorkspaceFactory`] returns. Boxed by hand rather than through
/// `futures`, which this crate does not depend on.
pub type WorkspaceFuture = Pin<Box<dyn Future<Output = Result<Option<WorkerWorkspace>>> + Send>>;

/// What one worker's directory is made from: its participant name, and the
/// part of its [`SpawnContext`] that says where (BI-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRequest {
    /// The worker's participant name, which names its tree.
    pub worker: String,
    /// The tree to make it under: the caller's own. `None` isolates nothing.
    pub workspace_root: Option<String>,
    /// What its branch starts from and its evidence diffs against: the
    /// caller's own branch. `None` is the root's `HEAD` and default branch.
    pub base_branch: Option<String>,
    /// The team's verification command for this worker (AGE-406).
    pub verification: Option<String>,
}

/// Makes a directory for one worker.
///
/// Async because making one is a `git worktree add` — a subprocess, not a
/// `mkdir`. `Ok(None)` means "no isolation available" and the child runs
/// where the broker does; that is a real fallback, not an error, because a
/// workspace that is not a git repository has no worktree to give (AGE-314's
/// open question).
pub type WorkspaceFactory =
    Arc<dyn Fn(WorkspaceRequest) -> WorkspaceFuture + Send + Sync + 'static>;

/// The descriptor a worker finds its connection on. The runner places the
/// worker's end of the socket pair there in the child, between fork and
/// exec, and says so with `--participant-fd`. Fixed rather than whatever the
/// parent's descriptor happened to be, so a stand-in worker written in `sh`
/// can speak on it (`>&3`).
pub const PARTICIPANT_FD: RawFd = 3;

/// How long a child gets to say hello before the task fails.
///
/// Generous, because it covers process start-up on a cold page cache; a real
/// hang is caught by the caller's own HTTP timeout, not by shaving this.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the runner checks whether the child has said hello yet.
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
    /// Extra arguments every child gets — the model id, `--auto-approve`.
    args: Vec<String>,
    workspace: Option<WorkspaceFactory>,
    /// The root's workspace: where a task given without a spawn context
    /// gets its tree (BI-5).
    workspace_root: Option<String>,
    /// The team's verification command for this agent, from the root's
    /// settings (AGE-406).
    verification: Option<String>,
    registry: ParticipantRegistry,
    /// The model endpoint its workers use, and the budget that meters it.
    /// `None` leaves the runner unmetered.
    endpoint: Option<(String, EndpointBudget)>,
    registration_timeout: Duration,
    /// The schema this role's answers must match, when the team names one
    /// (TD-2, AGE-693). Every task this runner starts carries it.
    handoff: Option<HandoffContract>,
}

impl LocalRunner {
    /// `executable` is the chatty binary to spawn; `registry` is the one the
    /// gateway serves, so a spawned worker is reachable by name.
    pub fn new(executable: impl Into<PathBuf>, registry: ParticipantRegistry) -> Self {
        Self {
            agent_name: "local-agent".to_string(),
            description: "A chatty agent in its own process, with its own \
                          workspace and tool set. Delegate a self-contained \
                          task to it and it works autonomously and reports back."
                .to_string(),
            executable: executable.into(),
            args: Vec::new(),
            workspace: None,
            workspace_root: None,
            verification: None,
            registry,
            endpoint: None,
            registration_timeout: REGISTRATION_TIMEOUT,
            handoff: None,
        }
    }

    /// The handoff contract every worker of this role answers under (TD-2).
    pub fn with_handoff(mut self, handoff: Option<HandoffContract>) -> Self {
        self.handoff = handoff;
        self
    }

    /// Rename the virtual agent callers address.
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    /// The card's description: what this runner's workers run — their
    /// model and tool set — so a caller reading the card can choose between
    /// runners rather than guess (ADR-0011 C10).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
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

    /// The root's workspace, under which a worker spawned for the root gets
    /// its tree (BI-5). A sub-leader's worker gets its tree under the
    /// sub-leader's, from the task's spawn context.
    pub fn with_workspace_root(mut self, root: Option<String>) -> Self {
        self.workspace_root = root;
        self
    }

    /// The team's verification command for this agent's workers (AGE-406).
    pub fn with_verification(mut self, command: Option<String>) -> Self {
        self.verification = command;
        self
    }

    /// Meter this runner's workers against `endpoint` — the base URL of the
    /// model server they will all talk to (ADR-0011 C6).
    ///
    /// The budget is shared, so two runners naming the same endpoint queue
    /// against each other, which is the point: the limit belongs to the
    /// model server, not to the caller.
    pub fn with_endpoint_budget(
        mut self,
        endpoint: impl Into<String>,
        budget: EndpointBudget,
    ) -> Self {
        self.endpoint = Some((endpoint.into(), budget));
        self
    }

    /// How long a child gets to say hello before the task fails.
    /// Defaults to [`REGISTRATION_TIMEOUT`].
    pub fn with_registration_timeout(mut self, timeout: Duration) -> Self {
        self.registration_timeout = timeout;
        self
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// The registry its workers' connections are registered in — the same
    /// one the gateway serves, so a spawned worker is reachable by name like
    /// any other.
    pub fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    /// The model endpoint its workers are metered against, if any.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_ref().map(|(name, _)| name.as_str())
    }

    /// How many tasks are waiting for a slot on this runner's endpoint.
    /// Zero when the runner is unmetered.
    pub fn queue_depth(&self) -> usize {
        match self.endpoint.as_ref() {
            Some((endpoint, budget)) => budget.queue_depth(endpoint),
            None => 0,
        }
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

    /// Spawn a worker and hand it `task`.
    ///
    /// The returned [`Worker`] owns the child process: dropping it reaps the
    /// child and runs the workspace's `on_exit`, so a caller that hangs up
    /// mid-task does not leak a process or an uncommitted worktree. It is
    /// returned alongside the task's update stream rather than owning it, so
    /// the caller can read updates while still holding the process handle.
    pub async fn run_task(&self, task: DelegatedTask) -> Result<(Worker, TaskStream)> {
        // Before the name, the workspace and the process: a queued task that
        // had already claimed those would be holding a worktree open for as
        // long as it waits.
        let permit = match self.endpoint.as_ref() {
            Some((endpoint, budget)) => Some(RunPermit::acquire(budget, endpoint).await),
            None => None,
        };

        // The broker names the node and makes its connection; the child
        // gets the other end and nothing to claim (ADR-0020). Its owner is
        // the calling node the broker stamped on the task (DP-2), or the
        // root when the root called or no broker call started it. Dropping
        // `worker_end` on any early return below closes the connection,
        // which abandons the node.
        let spawner = task.call.as_ref().and_then(|call| call.caller.as_deref());
        let LocalConnection { name, worker_end } =
            open_connection(&self.registry, &self.agent_name, spawner)
                .context("failed to make a connection for a worker")?;
        // The node's calls release its permit while they are outstanding
        // and re-acquire it before the last result is delivered (BI-6), so
        // the broker's call path finds it by the node's name.
        if let Some(permit) = permit.as_ref() {
            self.registry.set_node_permit(&name, permit);
        }
        // Where it goes comes from the task's spawn context: the caller's
        // own tree and branch (BI-5). A task without one is the root's.
        let request = match task.spawn_context.as_ref() {
            Some(context) => WorkspaceRequest {
                worker: name.clone(),
                workspace_root: context.workspace_root.clone(),
                base_branch: context.base_branch.clone(),
                verification: context.verification.clone(),
            },
            None => WorkspaceRequest {
                worker: name.clone(),
                workspace_root: self.workspace_root.clone(),
                base_branch: None,
                verification: self.verification.clone(),
            },
        };
        let workspace = match self.workspace.as_ref() {
            Some(factory) => factory(request.clone())
                .await
                .with_context(|| format!("failed to prepare a workspace for worker '{name}'"))?,
            None => None,
        };
        // The node's own context, which its calls spawn from: its tree and
        // branch, or its caller's when it got none of its own, and the
        // roster it was given.
        if let Some(context) = task.spawn_context.as_ref() {
            let (tree, branch) = match workspace.as_ref() {
                Some(w) => (Some(w.cwd.to_string_lossy().into_owned()), w.branch.clone()),
                None => (request.workspace_root, request.base_branch),
            };
            self.registry.set_node_context(
                &name,
                SpawnContext {
                    workspace_root: tree,
                    base_branch: branch,
                    roster: context.roster.clone(),
                    verification: None,
                    endpoint: None,
                },
            );
        }

        let mut child = self.spawn(&name, &worker_end, workspace.as_ref())?;
        // The child holds its copy. The broker must not keep one: a dead
        // child has to read as a closed connection.
        drop(worker_end);
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
            _permit: permit,
        };

        self.await_registration(&mut worker).await?;

        // The role's handoff contract rides the task (TD-2).
        let task = task.with_handoff(self.handoff.clone());
        let (task_id, updates) = self
            .registry
            .submit_task(&name, task)
            .await
            .ok_or_else(|| anyhow!("worker '{name}' disconnected before it could be given work"))?;
        worker.task_id = Some(task_id.clone());

        info!(worker = %name, task = %task_id, "Delegated a task to a local worker");
        Ok((worker, updates))
    }

    fn spawn(
        &self,
        name: &str,
        worker_end: &std::os::unix::net::UnixStream,
        workspace: Option<&WorkerWorkspace>,
    ) -> Result<Child> {
        let fd = worker_end.as_raw_fd();
        if fd <= 2 {
            // The child's stdio is set up before `pre_exec` runs and would
            // overwrite it. Only a parent running with a closed stdio
            // descriptor gets here.
            bail!("the worker's connection landed on stdio descriptor {fd}");
        }
        let mut cmd = Command::new(&self.executable);
        cmd.args(&self.args)
            .arg("--participant-fd")
            .arg(PARTICIPANT_FD.to_string())
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

        // SAFETY: the closure runs in the child between fork and exec, where
        // only async-signal-safe calls are allowed; it makes two (`dup2`,
        // `fcntl`) and touches no memory the parent's other threads could
        // have been holding a lock on.
        unsafe {
            cmd.pre_exec(move || place_participant_fd(fd));
        }

        debug!(worker = %name, exe = ?self.executable, "Spawning a local worker");
        cmd.spawn()
            .with_context(|| format!("failed to spawn {}", self.executable.display()))
    }

    /// Wait for the child's `hello`, or fail with why it did not come.
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

/// In the child, between fork and exec: put the worker's end of the socket
/// pair at [`PARTICIPANT_FD`] and let it survive the exec. Every other copy
/// of it is close-on-exec, so this is the only process that inherits it.
fn place_participant_fd(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: plain descriptor syscalls on descriptors this process owns.
    unsafe {
        if fd != PARTICIPANT_FD && libc::dup2(fd, PARTICIPANT_FD) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // `dup2` clears close-on-exec on the copy, but a descriptor that was
        // already at `PARTICIPANT_FD` still has it, so clear it either way.
        let flags = libc::fcntl(PARTICIPANT_FD, libc::F_GETFD);
        if flags == -1
            || libc::fcntl(PARTICIPANT_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
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

    fn workspace_root(&self) -> Option<&str> {
        self.workspace_root.as_deref()
    }

    fn verification(&self) -> Option<&str> {
        self.verification.as_deref()
    }

    fn endpoint(&self) -> Option<&str> {
        LocalRunner::endpoint(self)
    }

    fn run_task(&self, task: DelegatedTask) -> WorkerFuture<'_> {
        Box::pin(async move {
            let (worker, updates) = LocalRunner::run_task(self, task).await?;
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
    /// This worker's run's slot on the model endpoint (ADR-0011 C6),
    /// released while the run waits on its own calls (BI-6) and for good
    /// once it is reaped, so the next queued run is admitted by the same
    /// event that frees the process and its workspace.
    _permit: Option<RunPermit>,
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

    /// Takes the collector out of the workspace and runs it, so a second
    /// call is `None` rather than a second commit.
    fn evidence(&mut self) -> EvidenceFuture {
        match self.workspace.as_mut().and_then(|w| w.evidence.take()) {
            Some(collect) => collect(),
            None => Box::pin(std::future::ready(None)),
        }
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
    use std::sync::atomic::{AtomicBool, Ordering};

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
        LocalRunner::new(exe, registry)
            .with_args(args)
            .with_registration_timeout(Duration::from_secs(5))
    }

    /// A stand-in worker's script: say hello on the connection the runner
    /// handed it, then `then`.
    fn hello(then: &str) -> String {
        format!(
            "printf '{{\"v\":3,\"id\":1,\"method\":\"session.hello\",\"params\":{{}}}}\\n' >&{PARTICIPANT_FD}; {then}"
        )
    }

    /// A stand-in that says hello and then waits, like a worker mid-task.
    /// `exec`, so the process the runner reaps is the one holding the
    /// connection.
    fn waiting_worker() -> String {
        hello("exec sleep 30")
    }

    /// Every line the broker sent a stand-in that copies its connection to
    /// `path`, once one of them contains `needle`.
    async fn frames_containing(path: &std::path::Path, needle: &str) -> Vec<serde_json::Value> {
        for _ in 0..500 {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if text.contains(needle) {
                return text
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("the broker sends JSON"))
                    .collect();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the stand-in never received a frame containing {needle}");
    }

    #[tokio::test]
    async fn a_worker_that_says_hello_is_welcomed_by_name_and_handed_the_task() {
        let registry = ParticipantRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        let frames = dir.path().join("frames");
        let runner = runner(
            registry.clone(),
            &hello(&format!(
                "exec cat <&{PARTICIPANT_FD} > '{}'",
                frames.display()
            )),
        );

        let (worker, _updates) = runner
            .run_task(DelegatedTask::from_root("summarise foo.rs"))
            .await
            .expect("the worker said hello, so the task is delegated");

        assert_eq!(worker.name(), "local-agent-0", "the broker names it");
        assert!(registry.is_registered("local-agent-0"));
        let frames = frames_containing(&frames, "summarise foo.rs").await;
        assert_eq!(frames[0]["v"], 3);
        assert_eq!(frames[0]["id"], 1, "the hello's result");
        assert_eq!(frames[0]["result"]["name"], "local-agent-0");
        assert_eq!(frames[1]["method"], "task.run", "the welcome comes first");
        assert_eq!(frames[1]["params"]["text"], "summarise foo.rs");
        assert_eq!(frames[1]["params"]["taskId"].as_str(), worker.task_id());
    }

    /// The child gets its end of the connection at `PARTICIPANT_FD`, open
    /// across the exec, and is told where with `--participant-fd`.
    #[tokio::test]
    async fn the_child_is_told_its_descriptor_and_finds_a_socket_there() {
        let registry = ParticipantRegistry::new();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let runner = runner(
            registry,
            &format!(
                "printf '%s|' \"$0 $*\" > '{out}'; [ -S /dev/fd/{PARTICIPANT_FD} ] && echo socket >> '{out}'; {}",
                waiting_worker(),
                out = out.display(),
            ),
        );

        let _worker = runner
            .run_task(DelegatedTask::from_root("x"))
            .await
            .unwrap();
        let out = std::fs::read_to_string(&out).unwrap();
        assert!(
            out.contains(&format!("--participant-fd {PARTICIPANT_FD}|")),
            "{out}"
        );
        assert!(out.ends_with("socket\n"), "{out}");
        assert!(!out.contains("--participant-name"), "{out}");
    }

    #[tokio::test]
    async fn a_worker_that_dies_before_registering_reports_why() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry, "echo 'no model configured' >&2; exit 3");

        let error = runner
            .run_task(DelegatedTask::from_root("anything"))
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
            .run_task(DelegatedTask::from_root("anything"))
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

        let runner = runner(registry.clone(), &waiting_worker()).with_workspace_factory({
            let cwd = dir.path().to_path_buf();
            let released = released.clone();
            Arc::new(move |request: WorkspaceRequest| {
                assert_eq!(
                    request.worker, "local-agent-0",
                    "the factory is told who it is for"
                );
                let cwd = cwd.clone();
                let released = released.clone();
                Box::pin(async move {
                    Ok(Some(WorkerWorkspace {
                        cwd,
                        branch: None,
                        evidence: None,
                        on_exit: Box::new(move |_| {
                            released.store(true, Ordering::Relaxed);
                        }),
                    }))
                })
            })
        });

        let (worker, _updates) = runner
            .run_task(DelegatedTask::from_root("work"))
            .await
            .unwrap();
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
        let runner = runner(registry, &waiting_worker()).with_workspace_factory(Arc::new(|_| {
            Box::pin(async { Err(anyhow!("the worktree could not be created")) })
        }));

        let error = runner
            .run_task(DelegatedTask::from_root("work"))
            .await
            .expect_err("ADR-0012 isolation that was asked for and failed is not silently skipped");
        assert!(format!("{error:#}").contains("worktree"), "{error:#}");
    }

    #[tokio::test]
    async fn dropping_a_worker_cancels_its_task() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry.clone(), &waiting_worker());

        let (worker, mut updates) = runner
            .run_task(DelegatedTask::from_root("work"))
            .await
            .unwrap();
        assert_eq!(registry.open_task_count("local-agent-0"), 1);

        drop(worker);

        assert_eq!(registry.open_task_count("local-agent-0"), 0);
        assert!(updates.recv().await.is_none(), "the caller's stream ends");
        // The reaped child's end closes, which deregisters the node.
        for _ in 0..500 {
            if !registry.is_registered("local-agent-0") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the reaped worker is still registered");
    }

    /// AGE-305's verification: a budget of two, three delegated tasks, and
    /// the third does not get a process until one of the first two is gone.
    #[tokio::test]
    async fn a_busy_endpoint_queues_the_third_worker_until_a_slot_frees() {
        const ENDPOINT: &str = "http://localhost:11434";

        let registry = ParticipantRegistry::new();
        let budget = EndpointBudget::new(2);
        let runner = Arc::new(
            runner(registry.clone(), &waiting_worker())
                .with_endpoint_budget(ENDPOINT, budget.clone()),
        );

        let first = runner
            .run_task(DelegatedTask::from_root("a"))
            .await
            .unwrap();
        let second = runner
            .run_task(DelegatedTask::from_root("b"))
            .await
            .unwrap();
        assert_eq!(budget.in_flight(ENDPOINT), 2, "the budget is spent");

        let third = tokio::spawn({
            let runner = Arc::clone(&runner);
            async move { runner.run_task(DelegatedTask::from_root("c")).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(!third.is_finished(), "the third task waits for a slot");
        assert_eq!(runner.queue_depth(), 1, "and the wait is visible");
        assert_eq!(
            registry.open_task_count("local-agent-2"),
            0,
            "a queued task has no child process yet"
        );

        // Reaping a worker releases its slot, and the queued task takes it.
        drop(first);
        let (worker, _updates) = tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("the queued task is admitted once a slot frees")
            .unwrap()
            .unwrap();

        assert_eq!(worker.name(), "local-agent-2");
        assert_eq!(runner.queue_depth(), 0);
        assert_eq!(budget.in_flight(ENDPOINT), 2);
        drop((second, worker));
        assert_eq!(budget.in_flight(ENDPOINT), 0, "every slot comes back");
    }

    #[tokio::test]
    async fn an_unmetered_runner_spawns_without_waiting() {
        let registry = ParticipantRegistry::new();
        let runner = runner(registry.clone(), &waiting_worker());

        let _first = runner
            .run_task(DelegatedTask::from_root("a"))
            .await
            .unwrap();
        let second = tokio::time::timeout(
            Duration::from_secs(5),
            runner.run_task(DelegatedTask::from_root("b")),
        )
        .await
        .expect("no budget, no queue");
        assert!(second.is_ok());
        assert_eq!(runner.queue_depth(), 0);
        assert!(runner.endpoint().is_none());
    }

    #[test]
    fn the_agent_card_describes_the_worker_it_would_spawn() {
        let runner = LocalRunner::new("/bin/sh", ParticipantRegistry::new());
        let card = runner.agent_card();
        assert_eq!(card.name, "local-agent");
        assert_eq!(card.skills[0].name, "delegate");
        assert!(!card.description.is_empty());
    }

    /// ADR-0011 C10: a named runner's card carries the name callers address
    /// and the text that says what its workers run.
    #[test]
    fn a_named_runner_serves_its_own_name_and_description() {
        let runner = LocalRunner::new("/bin/sh", ParticipantRegistry::new())
            .with_agent_name("local-reviewer")
            .with_description("Model: gemma. Tool groups disabled: fs-write.");
        let card = runner.agent_card();
        assert_eq!(card.name, "local-reviewer");
        assert_eq!(
            card.description,
            "Model: gemma. Tool groups disabled: fs-write."
        );
    }

    /// AGE-377's budget test, first half: two runners on different provider
    /// URLs hold independent permits — a reviewer on another server does
    /// not queue behind the coder.
    #[tokio::test]
    async fn runners_on_different_endpoints_hold_independent_permits() {
        let registry = ParticipantRegistry::new();
        let budget = EndpointBudget::new(1);
        let coder = runner(registry.clone(), &waiting_worker())
            .with_agent_name("local-coder")
            .with_endpoint_budget("http://localhost:11434", budget.clone());
        let reviewer = runner(registry.clone(), &waiting_worker())
            .with_agent_name("local-reviewer")
            .with_endpoint_budget("http://other:8000/v1", budget.clone());

        let first = coder
            .run_task(DelegatedTask::from_root("code"))
            .await
            .unwrap();
        assert_eq!(budget.in_flight("http://localhost:11434"), 1);

        let second = tokio::time::timeout(
            Duration::from_secs(5),
            reviewer.run_task(DelegatedTask::from_root("review")),
        )
        .await
        .expect("a different endpoint has its own slot, so nothing waits")
        .unwrap();
        assert_eq!(budget.in_flight("http://other:8000/v1"), 1);
        assert_eq!(reviewer.queue_depth(), 0);
        drop((first, second));
    }

    /// AGE-377's budget test, second half: two runners on one URL share one
    /// budget — the limit belongs to the model server, not to the caller.
    #[tokio::test]
    async fn runners_on_one_endpoint_share_its_budget() {
        const ENDPOINT: &str = "http://localhost:11434";

        let registry = ParticipantRegistry::new();
        let budget = EndpointBudget::new(1);
        let coder = runner(registry.clone(), &waiting_worker())
            .with_agent_name("local-coder")
            .with_endpoint_budget(ENDPOINT, budget.clone());
        let reviewer = Arc::new(
            runner(registry.clone(), &waiting_worker())
                .with_agent_name("local-reviewer")
                .with_endpoint_budget(ENDPOINT, budget.clone()),
        );

        let first = coder
            .run_task(DelegatedTask::from_root("code"))
            .await
            .unwrap();
        assert_eq!(budget.in_flight(ENDPOINT), 1, "the one slot is spent");

        let second = tokio::spawn({
            let reviewer = Arc::clone(&reviewer);
            async move { reviewer.run_task(DelegatedTask::from_root("review")).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !second.is_finished(),
            "the reviewer waits for the coder's slot on the same server"
        );
        assert_eq!(reviewer.queue_depth(), 1);

        drop(first);
        let (worker, _updates) = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .expect("the queued reviewer is admitted once the coder is reaped")
            .unwrap()
            .unwrap();
        assert_eq!(worker.name(), "local-reviewer-0");
        assert_eq!(budget.in_flight(ENDPOINT), 1);
    }

    /// BI-6: a metered worker's permit is its node's, where the broker's
    /// call path releases it while the worker's calls are outstanding. It
    /// is gone with the node.
    #[tokio::test]
    async fn a_metered_worker_s_permit_is_on_its_node() {
        const ENDPOINT: &str = "http://localhost:11434";

        let registry = ParticipantRegistry::new();
        let budget = EndpointBudget::new(1);
        let runner = runner(registry.clone(), &waiting_worker())
            .with_endpoint_budget(ENDPOINT, budget.clone());

        let (worker, _updates) = runner
            .run_task(DelegatedTask::from_root("a"))
            .await
            .unwrap();
        let permit = registry
            .node_permit(worker.name())
            .expect("the node's permit is recorded");
        assert_eq!(permit.endpoint(), ENDPOINT);
        assert_eq!(permit.state(), chatty_fabric::RunPermitState::Holding);

        let call = permit.child_call();
        assert_eq!(budget.in_flight(ENDPOINT), 0, "a waiting run holds no slot");
        call.finish().await;
        assert_eq!(budget.in_flight(ENDPOINT), 1);
        drop(permit);

        let name = worker.name().to_string();
        drop(worker);
        for _ in 0..500 {
            if registry.node_permit(&name).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            registry.node_permit(&name).is_none(),
            "forgotten with the node"
        );
        assert_eq!(budget.in_flight(ENDPOINT), 0, "and its slot is free");
    }
}
