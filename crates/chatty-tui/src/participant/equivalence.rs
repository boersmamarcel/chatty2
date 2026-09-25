//! AGE-301's verification: the parent's trace of a delegated task carries
//! every tool call the child reported, in order.
//!
//! ADR-0011's first kill criterion says A2A's task model must carry a
//! delegated turn at the granularity the parent already renders. "The same"
//! is made precise here: for every scripted scenario both frontends are
//! characterized against, the parent's progress lines are compared against
//! the child's own events put through `progress_text_for_event` — the
//! rendering both ends share.
//!
//! The broker path runs end to end: a real `ProtocolGateway` on a real port,
//! a real `A2aClient` inside a real `InvokeAgentTool`. Only the child's turn
//! is scripted, because that is the input both sides share.
//!
//! # Text is extra
//!
//! The broker also carries the assistant's **text** as artifact chunks, so
//! the parent sees the answer stream in while the turn runs.
//! `progress_text_for_event` says nothing about text, so the comparison
//! subtracts exactly those chunks and asserts the remainder is identical —
//! the broker is a superset of what the child reported, never a subset,
//! which is the direction the kill criterion cares about.

use std::collections::HashMap;
use std::sync::Arc;

use chatty_core::models::token_usage::TokenUsage;
use chatty_core::services::{
    Scenario, StreamSurface, clarification_scenario, install_progress_channel, scenarios,
};
use chatty_core::session::{SessionEvent, TurnPolicy, replay_scenario};
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_core::tools::{LOCAL_AGENT_NAME, progress_text_for_event};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{
    AgentOrigin, BrokerFrame, ParticipantCard, ParticipantFrame, ParticipantRegistry,
};
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::{RwLock, mpsc};

use chatty_protocol_gateway::worker::TaskMapper;

/// The policy a delegated child runs its turn under.
fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::Headless,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
        think_disabled: false,
    }
}

// ---------------------------------------------------------------------------
// The reference rendering: the child's own events, rendered
// ---------------------------------------------------------------------------

/// The progress lines the child's events amount to.
///
/// Every event is offered: `progress_text_for_event` is the filter, and it
/// ignores the ones that say nothing about tool activity (`Text`,
/// `TurnMessages`).
fn reference_trace(events: &[SessionEvent]) -> Vec<String> {
    let mut names = HashMap::new();
    events
        .iter()
        .filter_map(|event| progress_text_for_event(event, &mut names))
        .collect()
}

/// The assistant text of a scripted turn, which the broker streams as
/// artifact chunks.
fn assistant_text(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The broker path: a gateway, a participant, and the real invoke_agent
// ---------------------------------------------------------------------------

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("noop".into())
    }
}

/// Start a gateway on an ephemeral port and return its port and registry.
async fn start_gateway() -> (u16, ParticipantRegistry) {
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));
    let gateway = ProtocolGateway::new(modules, 0);
    let participants = gateway.participants();

    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });

    (port, participants)
}

/// Register a participant under `name` that answers its one task by
/// replaying `events` through the mapping under test.
fn spawn_scripted_worker(registry: &ParticipantRegistry, name: &str, events: Vec<SessionEvent>) {
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<BrokerFrame>();
    registry
        .register(
            ParticipantCard {
                name: name.to_string(),
                description: "a scripted worker".to_string(),
                ..Default::default()
            },
            AgentOrigin::Local,
            outbound_tx,
        )
        .expect("the scripted worker registers");

    let registry = registry.clone();
    let name = name.to_string();
    tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            let BrokerFrame::Task { task_id, .. } = frame else {
                continue;
            };
            let mut mapper = TaskMapper::new(task_id);
            for event in &events {
                if let Some(frame) = mapper.map(event) {
                    registry.on_frame(&name, frame);
                }
            }
            registry.on_frame(&name, mapper.terminal());
            return;
        }
    });
}

/// The parent's progress, and the response `invoke_agent` hands the model.
struct BrokerRun {
    progress: Vec<String>,
    response: String,
    succeeded: bool,
    /// What the worker reported spending, as the tool's `Finished` carries
    /// it to the parent's session (AGE-415).
    usage: Option<TokenUsage>,
}

/// Delegate one task through the real `invoke_agent` tool and record what the
/// parent saw.
async fn broker_run(events: Vec<SessionEvent>) -> BrokerRun {
    let (port, registry) = start_gateway().await;
    spawn_scripted_worker(&registry, LOCAL_AGENT_NAME, events);

    let tool =
        InvokeAgentTool::new(vec![], vec![], Some(port)).with_local_agents([LOCAL_AGENT_NAME]);
    let mut progress_rx = install_progress_channel(&tool.progress_slot());

    let result = tool
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: LOCAL_AGENT_NAME.to_string(),
                prompt: "do the delegated task".to_string(),
                include_trace: false,
            },
        )
        .await;

    let mut progress = Vec::new();
    let mut usage = None;
    while let Ok(event) = progress_rx.try_recv() {
        match event {
            InvokeAgentProgress::Text(text) => progress.push(text),
            InvokeAgentProgress::Finished {
                usage: reported, ..
            } => usage = reported,
            InvokeAgentProgress::Started { .. } => {}
        }
    }

    BrokerRun {
        progress,
        response: result
            .as_ref()
            .map(|o| o.response.clone())
            .unwrap_or_default(),
        succeeded: result.is_ok(),
        usage,
    }
}

/// Remove the artifact chunks — the assistant's text — so what is left is
/// comparable with [`reference_trace`].
fn without_text_chunks(progress: &[String], text: &[String]) -> Vec<String> {
    let mut remaining: Vec<&String> = text.iter().collect();
    progress
        .iter()
        .filter(|line| match remaining.iter().position(|t| *t == *line) {
            Some(at) => {
                remaining.remove(at);
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The issue's "Verify", over every scenario both frontends are pinned to.
#[tokio::test]
async fn a_delegated_task_renders_every_tool_call_the_child_reported() {
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        let events = replay_scenario(scenario, policy()).await;

        let expected = reference_trace(&events);
        let run = broker_run(events.clone()).await;
        let over_the_broker = without_text_chunks(&run.progress, &assistant_text(&events));

        assert_eq!(
            expected, over_the_broker,
            "scenario '{name}': the parent's tool-call trace differs from what \
             the child reported.\n  expected: {expected:?}\n  broker: {:?}",
            run.progress
        );
    }
}

/// The answer itself reaches the parent model, not just the progress lines.
#[tokio::test]
async fn the_delegated_answer_reaches_the_parent_model() {
    let scenario = scenarios()
        .into_iter()
        .find(|s: &Scenario| s.name == "tool_call_then_result")
        .expect("the scenario exists");
    let events = replay_scenario(scenario, policy()).await;
    let expected: String = assistant_text(&events).concat();
    assert!(!expected.is_empty(), "the scenario has an answer to carry");

    let run = broker_run(events).await;

    assert!(run.succeeded, "the delegation succeeded");
    assert_eq!(run.response, expected.trim());
}

/// A per-tool trace is the thing the criterion is about, so assert it
/// concretely rather than only against the other path.
#[tokio::test]
async fn the_parent_sees_each_tool_start_and_finish_in_order() {
    let scenario = scenarios()
        .into_iter()
        .find(|s: &Scenario| s.name == "tool_call_then_result")
        .expect("the scenario exists");
    let events = replay_scenario(scenario, policy()).await;
    let run = broker_run(events.clone()).await;
    let trace = without_text_chunks(&run.progress, &assistant_text(&events));

    assert_eq!(
        trace,
        vec!["read_file".to_string(), "\u{2713} read_file".to_string()],
        "the parent renders the tool starting and then finishing"
    );
}

/// The comparison is only worth anything if a changed mapping breaks it.
#[tokio::test]
async fn a_dropped_tool_event_would_be_caught() {
    let events = replay_scenario(
        scenarios()
            .into_iter()
            .find(|s: &Scenario| s.name == "tool_call_then_result")
            .expect("the scenario exists"),
        policy(),
    )
    .await;

    let expected = reference_trace(&events);
    let mut mapper = TaskMapper::new("task-1");
    let mapped: Vec<String> = events
        .iter()
        // Pretend the mapping forgot tool results.
        .filter(|e| !matches!(e, SessionEvent::ToolCallResult { .. }))
        .filter_map(|e| mapper.map(e))
        .filter_map(|frame| match frame {
            ParticipantFrame::Status {
                message: Some(text),
                ..
            } => Some(text),
            _ => None,
        })
        .collect();

    assert_ne!(
        expected, mapped,
        "a mapping that drops tool results must not compare equal"
    );
}

// ---------------------------------------------------------------------------
// The bill follows the bearer (AGE-415)
// ---------------------------------------------------------------------------

mod delegated_usage {
    //! What a worker spent reaches its leader as one usage line named for
    //! the worker, and a sub-leader's line already carries its own workers,
    //! so the root of a tree sees one number per delegation. The session
    //! side — that line priced onto the leader's conversation — is pinned
    //! in chatty-core's session tests; this is the hop.

    use super::*;

    fn usage(input: u32, output: u32, read: u32, write: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
            ..Default::default()
        }
    }

    /// A worker's turn: some text, its usage, and the end.
    fn worker_turn(own: TokenUsage) -> Vec<SessionEvent> {
        vec![
            SessionEvent::TurnStarted,
            SessionEvent::Text("done".into()),
            SessionEvent::TokenUsage(own),
            SessionEvent::TurnEnded,
        ]
    }

    #[tokio::test]
    async fn a_workers_usage_reaches_the_leader_named_for_the_worker() {
        let run = broker_run(worker_turn(usage(1_200, 80, 900, 40))).await;

        assert!(run.succeeded);
        let reported = run.usage.expect("the worker's usage reaches the leader");
        assert_eq!(reported.delegated_to.as_deref(), Some(LOCAL_AGENT_NAME));
        assert_eq!(reported.input_tokens, 1_200);
        assert_eq!(reported.output_tokens, 80);
        assert_eq!(reported.cache_read_tokens, 900);
        assert_eq!(reported.cache_write_tokens, 40);
    }

    /// A sub-leader's own delegations are already in the number it reports,
    /// so the root sees the tree's spend as one line, not a flat list.
    #[tokio::test]
    async fn a_sub_leaders_line_includes_its_own_workers() {
        let mut events = vec![SessionEvent::TurnStarted];
        for (name, spent) in [
            ("local-coder", usage(5_000, 500, 0, 0)),
            ("local-reviewer", usage(2_000, 100, 1_000, 0)),
        ] {
            events.push(SessionEvent::Delegation(InvokeAgentProgress::Finished {
                success: true,
                result: Some("ok".into()),
                usage: Some(TokenUsage {
                    delegated_to: Some(name.into()),
                    ..spent
                }),
            }));
        }
        events.extend(worker_turn(usage(300, 30, 0, 10)).into_iter().skip(1));

        let run = broker_run(events).await;

        assert!(run.succeeded);
        let reported = run.usage.expect("the sub-leader's usage reaches the root");
        assert_eq!(
            reported.delegated_to.as_deref(),
            Some(LOCAL_AGENT_NAME),
            "one line, named for the agent the root delegated to"
        );
        assert_eq!(reported.input_tokens, 300 + 5_000 + 2_000);
        assert_eq!(reported.output_tokens, 30 + 500 + 100);
        assert_eq!(reported.cache_read_tokens, 1_000);
        assert_eq!(reported.cache_write_tokens, 10);
    }

    /// A worker that reports no usage adds no line: nothing is invented.
    #[tokio::test]
    async fn a_worker_that_reports_nothing_adds_no_line() {
        let run = broker_run(vec![
            SessionEvent::TurnStarted,
            SessionEvent::Text("done".into()),
            SessionEvent::TurnEnded,
        ])
        .await;
        assert!(run.succeeded);
        assert!(run.usage.is_none());
    }
}

// ---------------------------------------------------------------------------
// The runner's evidence envelope reaches the parent (AGE-399 / AGE-406)
// ---------------------------------------------------------------------------

mod evidence {
    //! ADR-0011 C12's verification: when a worker's task ends, the *runner*
    //! — not the model — records the branch, the diff stat, the commit
    //! count and the team's verification result, and appends them to the
    //! answer as a fenced `evidence` block. A worker that committed nothing
    //! gets none, so a read-only reviewer is never handed a branch to
    //! merge.
    //!
    //! Exercising it needs a real `LocalRunner` over a real `git worktree`,
    //! not the bare registered participant `broker_run` above uses — only a
    //! runner's `WorkerWorkspace` carries an envelope collector. The worker
    //! itself is still the scripted stand-in `spawn_scripted_worker` sets
    //! up, registered under the name the runner deterministically allocates
    //! its first worker (`local-agent-0`); what it *left in its tree* is
    //! seeded by the workspace factory, since a scripted worker edits
    //! nothing of its own.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use chatty_core::services::worker_tree;
    use chatty_core::services::{
        StreamError, StreamErrorKind, install_progress_channel, scenarios,
    };
    use chatty_core::session::{SessionEvent, replay_scenario};
    use chatty_core::tools::LOCAL_AGENT_NAME;
    use chatty_core::tools::invoke_agent_tool::{
        InvokeAgentArgs, InvokeAgentProgress, InvokeAgentTool,
    };
    use chatty_module_registry::ModuleRegistry;
    use chatty_protocol_gateway::ProtocolGateway;
    use chatty_protocol_gateway::participant::{
        LocalRunner, ParticipantRegistry, TaskEvidence, WorkerWorkspace,
    };
    use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
    use rig_agent::tool::{Tool, ToolContext};
    use tokio::sync::RwLock;

    use super::{NoopProvider, assistant_text, policy, spawn_scripted_worker};

    const FIRST_WORKER: &str = "local-agent-0";
    const FIRST_BRANCH: &str = "sub-agent/local-agent-0";
    /// Exit code 3 rather than 0 or 1: it can only have come from actually
    /// running the command.
    const VERIFICATION: &str = "echo 'ran the suite'; exit 3";

    /// A repository with one commit on `main`, so a worker's branch has a
    /// default branch to be measured against.
    async fn repo(dir: &Path) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        std::fs::write(dir.join("README"), "hi").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
    }

    /// A gateway whose `local-agent` is a real `LocalRunner` giving its
    /// worker a real `git worktree` under `root`, wired exactly as
    /// `broker.rs`'s `worktree_factory` wires it.
    ///
    /// `edits` is written into the worker's tree the moment it is made: the
    /// scripted stand-in worker writes nothing itself, so this is what
    /// tells "a coder that commits" from "a reviewer that does not".
    async fn start_runner_gateway(
        root: PathBuf,
        verification: Option<String>,
        edits: bool,
    ) -> (u16, ParticipantRegistry) {
        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let modules = Arc::new(RwLock::new(
            ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
        ));
        let gateway = ProtocolGateway::new(modules, 0);
        let registry = gateway.participants();

        let runner = LocalRunner::new(
            "/bin/sh",
            "/nonexistent/participants.sock",
            registry.clone(),
        )
        .with_agent_name(LOCAL_AGENT_NAME)
        .with_args(["-c", "sleep 30"])
        .with_registration_timeout(Duration::from_secs(5))
        .with_workspace_factory(Arc::new(move |worker: String| {
            let root = root.to_string_lossy().to_string();
            let verification = verification.clone();
            Box::pin(async move {
                let (cwd, evidence, on_exit) =
                    worker_tree::create_with_commit_hook(&root, &worker, verification)
                        .await?
                        .expect("the workspace is a git repository");
                if edits {
                    std::fs::write(cwd.join("added.rs"), "fn added() {}\n").unwrap();
                }
                Ok(Some(WorkerWorkspace {
                    cwd,
                    evidence: Some(Box::new(move || {
                        Box::pin(async move {
                            evidence().await.map(|found| TaskEvidence {
                                text: found.block(),
                                data: found.json(),
                            })
                        })
                    })),
                    on_exit,
                }))
            })
        }));
        let gateway = gateway.with_virtual_agent(Arc::new(runner));

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let router = gateway.build_router();
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        (port, registry)
    }

    /// Delegate one task to `local-agent` and return the answer the model
    /// reads plus every progress line the leader's transcript renders.
    async fn delegate(port: u16) -> (Result<String, String>, String) {
        let tool =
            InvokeAgentTool::new(vec![], vec![], Some(port)).with_local_agents([LOCAL_AGENT_NAME]);
        let mut progress_rx = install_progress_channel(&tool.progress_slot());

        let result = tool
            .call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: LOCAL_AGENT_NAME.to_string(),
                    prompt: "delegate this".to_string(),
                    include_trace: false,
                },
            )
            .await;

        let mut progress = String::new();
        while let Ok(event) = progress_rx.try_recv() {
            if let InvokeAgentProgress::Text(text) = event {
                progress.push_str(&text);
            }
        }
        (
            result.map(|r| r.response).map_err(|e| format!("{e:#}")),
            progress,
        )
    }

    async fn completed_turn() -> Vec<SessionEvent> {
        let scenario = scenarios()
            .into_iter()
            .find(|s| s.name == "tool_call_then_result")
            .expect("the scenario exists");
        replay_scenario(scenario, policy()).await
    }

    /// Do items 1 and 2, and the first acceptance criterion: a coder that
    /// commits gets an envelope naming its branch, its diff stat and the
    /// verification command's exit code — in the answer the model actually
    /// reads, not merely in the trace.
    #[tokio::test]
    async fn a_coder_that_commits_gets_an_envelope_with_branch_diff_and_verification() {
        let dir = tempfile::tempdir().expect("a workspace dir");
        repo(dir.path()).await;
        let (port, registry) =
            start_runner_gateway(dir.path().to_path_buf(), Some(VERIFICATION.into()), true).await;

        let events = completed_turn().await;
        spawn_scripted_worker(&registry, FIRST_WORKER, events.clone());

        let (response, progress) = delegate(port).await;
        let response = response.expect("the delegation succeeds");

        let answer: String = assistant_text(&events).concat();
        assert!(
            response.starts_with(answer.trim()),
            "the worker's own report still comes first: {response:?}"
        );
        assert!(response.contains("```evidence"), "{response}");
        assert!(
            response.contains(&format!("branch: {FIRST_BRANCH}")),
            "{response}"
        );
        assert!(response.contains("base: main"), "{response}");
        assert!(response.contains("commits: 1"), "{response}");
        assert!(
            response.contains("added.rs"),
            "the diff stat names what the worker changed: {response}"
        );
        assert!(
            response.contains("exit code 3") && response.contains("ran the suite"),
            "the verification result is the runner's, not the worker's: {response}"
        );

        // Third acceptance criterion: the block the leader's transcript
        // renders — the delegation's `output` payload (AGE-401) — carries
        // it exactly once, and so does everything streamed to get there.
        assert_eq!(
            response.matches("```evidence").count(),
            1,
            "the fenced block appears once in the leader's transcript: {response}"
        );
        assert_eq!(progress.matches("```evidence").count(), 1, "{progress}");
    }

    /// Second acceptance criterion: a reviewer commits nothing, so it gets
    /// no envelope and no hint. Nothing tells it there is a branch to take.
    #[tokio::test]
    async fn a_reviewer_that_commits_nothing_gets_no_envelope_and_no_hint() {
        let dir = tempfile::tempdir().expect("a workspace dir");
        repo(dir.path()).await;
        let (port, registry) =
            start_runner_gateway(dir.path().to_path_buf(), Some(VERIFICATION.into()), false).await;

        let events = completed_turn().await;
        spawn_scripted_worker(&registry, FIRST_WORKER, events.clone());

        let (response, progress) = delegate(port).await;
        let response = response.expect("the delegation succeeds");

        let answer: String = assistant_text(&events).concat();
        assert_eq!(
            response,
            answer.trim(),
            "a worker that committed nothing adds nothing to its own report"
        );
        for text in [&response, &progress] {
            assert!(!text.contains("evidence"), "{text}");
            assert!(!text.contains("sub-agent/"), "{text}");
            assert!(!text.contains("merge"), "{text}");
        }
    }

    /// A worker that failed still committed whatever it had, so the
    /// envelope must still reach the parent (here, its progress trace —
    /// `InvokeAgentTool` does not hand a failed call's text to the model at
    /// all, which is unrelated to this issue).
    #[tokio::test]
    async fn a_failed_delegation_still_reports_what_is_on_the_branch() {
        let dir = tempfile::tempdir().expect("a workspace dir");
        repo(dir.path()).await;
        let (port, registry) = start_runner_gateway(dir.path().to_path_buf(), None, true).await;

        spawn_scripted_worker(
            &registry,
            FIRST_WORKER,
            vec![SessionEvent::Error(StreamError {
                kind: StreamErrorKind::Other,
                message: "the worker crashed".to_string(),
            })],
        );

        let (result, progress) = delegate(port).await;
        assert!(
            result.is_err(),
            "the worker's own failure still fails the delegation"
        );
        assert!(
            progress.contains("```evidence") && progress.contains(FIRST_BRANCH),
            "a failed worker's partial edits are still on its branch: {progress:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Named virtual agents (ADR-0011 C10 / AGE-377)
// ---------------------------------------------------------------------------

pub(super) mod named_virtual_agents {
    //! AGE-377's verification: two declared agents, `local-coder` and
    //! `local-reviewer`, over the exact gateway `--broker` starts. Both
    //! appear in `list_agents` with their model in the card; a task to each
    //! spawns a child whose argv carries the expected `--model` and
    //! `--disable`; and a reviewer child built from those flags has no
    //! `write_file` tool in its schema.
    //!
    //! The child is a stand-in binary that records its argv and waits, and
    //! a scripted participant registered under the name the runner will
    //! allocate (`<agent>-0`) answers the task — the same split
    //! `runner.rs`'s own tests use. Only the argv is what this pins; what
    //! a real `chatty-tui` does with it is `apply_tool_overrides` and
    //! `resolve_model`, checked separately below.

    use std::path::PathBuf;
    use std::time::Duration;

    use chatty_core::factories::{AgentBuildContext, AgentClient, AgentServices};
    use chatty_core::models::clarification_store::ClarificationStore;
    use chatty_core::models::execution_approval_store::ExecutionApprovalStore;
    use chatty_core::models::write_approval_store::WriteApprovalStore;
    use chatty_core::services::virtual_agents::resolve_virtual_agents;
    use chatty_core::services::{StreamSurface, scenarios};
    use chatty_core::session::{SessionEvent, TurnPolicy, replay_scenario};
    use chatty_core::settings::models::models_store::ModelConfig;
    use chatty_core::settings::models::module_settings::VirtualAgentConfig;
    use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
    use chatty_core::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};
    use chatty_core::tools::invoke_agent_tool::{InvokeAgentArgs, InvokeAgentTool};
    use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};
    use chatty_protocol_gateway::participant::{
        AgentOrigin, BrokerFrame, ParticipantCard, ParticipantRegistry,
    };
    use chatty_protocol_gateway::worker::TaskMapper;
    use clap::Parser;
    use rig_agent::tool::{Tool, ToolContext};
    use tokio::sync::mpsc;

    use crate::participant::broker::Broker;

    const CODER: &str = "local-coder";
    const REVIEWER: &str = "local-reviewer";
    const REVIEWER_DISABLED: [&str; 3] = ["fs-write", "shell", "git"];

    /// The team the issue's manual run uses: a coder on one model, a
    /// reviewer on another that cannot edit.
    fn team() -> ModuleSettingsModel {
        ModuleSettingsModel {
            // Two slots, so the second delegation does not wait on the
            // first stand-in child being reaped; queueing is C6's test.
            default_endpoint_budget: 2,
            virtual_agents: vec![
                VirtualAgentConfig {
                    name: CODER.to_string(),
                    model: Some("qwen3:4b".to_string()),
                    ..VirtualAgentConfig::default()
                },
                VirtualAgentConfig {
                    name: REVIEWER.to_string(),
                    model: Some("gemma4:26b".to_string()),
                    disable_tools: REVIEWER_DISABLED.iter().map(|s| s.to_string()).collect(),
                    ..VirtualAgentConfig::default()
                },
            ],
            ..ModuleSettingsModel::default()
        }
    }

    fn roster() -> (Vec<ModelConfig>, Vec<ProviderConfig>) {
        let models = ["qwen3:4b", "gemma4:26b"]
            .iter()
            .map(|id| {
                ModelConfig::new(
                    id.to_string(),
                    id.to_string(),
                    ProviderType::Ollama,
                    id.to_string(),
                )
            })
            .collect();
        let mut ollama = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        ollama.base_url = Some("http://localhost:11434".to_string());
        (models, vec![ollama])
    }

    /// A "chatty-tui" that appends its argv to `argv.log` beside itself and
    /// then waits to be reaped, as a real child would wait on its task.
    pub(crate) fn stand_in_binary(dir: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("chatty-tui");
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/argv.log\"\nexec sleep 30\n",
        )
        .expect("the stand-in binary is written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("the stand-in binary is executable");
        path
    }

    /// The argv lines the stand-in children recorded so far, once there
    /// are at least `at_least` of them.
    pub(crate) async fn recorded_argv(dir: &std::path::Path, at_least: usize) -> Vec<String> {
        let log = dir.join("argv.log");
        for _ in 0..200 {
            let lines: Vec<String> = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            if lines.len() >= at_least {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the stand-in child never recorded its argv in {}",
            log.display()
        );
    }

    /// A broker exactly as `--broker` would start it for `team()`, with the
    /// leader's flags forwarded and the stand-in binary as the worker.
    async fn start_team_broker(dir: &std::path::Path, provider_flags: &[String]) -> Broker {
        let (models, providers) = roster();
        let settings = team();
        let mut common_args = vec!["--auto-approve".to_string()];
        common_args.extend(provider_flags.iter().cloned());
        let specs = resolve_virtual_agents(&models, &providers, &settings, &common_args);
        Broker::start_at(
            dir.join("participants.sock"),
            stand_in_binary(dir),
            settings.default_endpoint_budget,
            specs,
            None,
        )
        .await
        .expect("the broker starts with two declared agents")
    }

    /// Register a stand-in under `name` that answers its task only once the
    /// child spawned for it has recorded its argv — the runner reaps the
    /// child the moment the task ends, and the stand-in answers in
    /// microseconds, so without this gate `sh` could be killed before its
    /// first line runs. Otherwise `spawn_scripted_worker`.
    pub(crate) fn spawn_argv_gated_worker(
        registry: &ParticipantRegistry,
        name: &str,
        argv_log: PathBuf,
        events: Vec<SessionEvent>,
    ) {
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<BrokerFrame>();
        registry
            .register(
                ParticipantCard {
                    name: name.to_string(),
                    description: "a scripted worker".to_string(),
                    ..Default::default()
                },
                AgentOrigin::Local,
                outbound_tx,
            )
            .expect("the scripted worker registers");

        let registry = registry.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            while let Some(frame) = outbound_rx.recv().await {
                let BrokerFrame::Task { task_id, .. } = frame else {
                    continue;
                };
                let marker = format!("--participant-name {name}");
                for _ in 0..500 {
                    if std::fs::read_to_string(&argv_log)
                        .unwrap_or_default()
                        .contains(&marker)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                let mut mapper = TaskMapper::new(task_id);
                for event in &events {
                    if let Some(frame) = mapper.map(event) {
                        registry.on_frame(&name, frame);
                    }
                }
                registry.on_frame(&name, mapper.terminal());
                return;
            }
        });
    }

    /// Delegate one task to `agent` through the real `invoke_agent`, with a
    /// scripted stand-in answering under the name the runner allocates.
    async fn delegate(broker: &Broker, dir: &std::path::Path, agent: &str) {
        let events = replay_scenario(
            scenarios()
                .into_iter()
                .find(|s| s.name == "tool_call_then_result")
                .expect("the scenario exists"),
            TurnPolicy {
                surface: StreamSurface::Headless,
                max_agent_turns: 10,
                loop_guard: false,
                already_asked_to_retry: false,
                think_disabled: false,
            },
        )
        .await;
        spawn_argv_gated_worker(
            &broker.participants(),
            &format!("{agent}-0"),
            dir.join("argv.log"),
            events,
        );

        let tool = InvokeAgentTool::new(vec![], vec![], Some(broker.port))
            .with_local_agents([CODER, REVIEWER]);
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: agent.to_string(),
                prompt: format!("a task for {agent}"),
                include_trace: false,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("delegating to {agent} succeeds: {e:#}"));
    }

    /// The issue's "Verify": both declared agents are listed with their
    /// model in the card, and a task to each spawns a child whose argv
    /// carries its `--model` and `--disable`.
    #[tokio::test]
    async fn declared_agents_are_listed_with_their_model_and_spawned_with_their_flags() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let broker = start_team_broker(dir.path(), &[]).await;

        // list_agents, built as agent_factory builds it with the broker's
        // port and module settings' names.
        let output = ListAgentsTool::new(vec![])
            .with_local_workers([CODER, REVIEWER])
            .with_gateway_port(broker.port)
            .call(&mut ToolContext::new(), ListAgentsToolArgs {})
            .await
            .expect("list_agents succeeds");
        let find = |name: &str| {
            output
                .agents
                .iter()
                .find(|a| a.name == name)
                .unwrap_or_else(|| panic!("{name} is listed, got {:?}", output.agents))
        };
        assert!(
            find(CODER).description.contains("Model: qwen3:4b"),
            "{}",
            find(CODER).description
        );
        assert!(
            find(REVIEWER).description.contains("Model: gemma4:26b"),
            "{}",
            find(REVIEWER).description
        );
        assert!(
            find(REVIEWER)
                .description
                .contains("Tool groups disabled: fs-write, shell, git"),
            "{}",
            find(REVIEWER).description
        );

        delegate(&broker, dir.path(), CODER).await;
        delegate(&broker, dir.path(), REVIEWER).await;

        let argv = recorded_argv(dir.path(), 2).await;
        let coder = argv
            .iter()
            .find(|line| line.contains("--participant-name local-coder-0"))
            .unwrap_or_else(|| panic!("no coder child spawned: {argv:?}"));
        assert!(coder.contains("--model qwen3:4b"), "{coder}");
        assert!(
            !coder.contains("--disable"),
            "the coder keeps every tool: {coder}"
        );
        assert!(coder.contains("--auto-approve"), "{coder}");

        let reviewer = argv
            .iter()
            .find(|line| line.contains("--participant-name local-reviewer-0"))
            .unwrap_or_else(|| panic!("no reviewer child spawned: {argv:?}"));
        assert!(reviewer.contains("--model gemma4:26b"), "{reviewer}");
        assert!(
            reviewer.contains("--disable fs-write,shell,git"),
            "{reviewer}"
        );

        broker.shutdown();
    }

    /// The forwarding test (Do item 4): a C9 headless leader started with
    /// `--openai-compat-url http://x --api-key k` spawns children whose
    /// argv contains both flags.
    #[tokio::test]
    async fn a_flag_configured_leader_forwards_its_provider_flags_to_every_child() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let cli = crate::Cli::try_parse_from([
            "chatty-tui",
            "--headless",
            "--broker",
            "--openai-compat-url",
            "http://x",
            "--api-key",
            "k",
            "-m",
            "hi",
        ])
        .expect("the leader's flags parse");
        let flags = crate::participant::broker::provider_flags(
            cli.ollama.as_deref(),
            cli.openai_compat_url.as_deref(),
            cli.api_key.as_deref(),
        );
        let broker = start_team_broker(dir.path(), &flags).await;

        delegate(&broker, dir.path(), CODER).await;

        let argv = recorded_argv(dir.path(), 1).await;
        assert!(
            argv[0].contains("--openai-compat-url http://x"),
            "{}",
            argv[0]
        );
        assert!(argv[0].contains("--api-key k"), "{}", argv[0]);
        assert!(argv[0].contains("--model qwen3:4b"), "{}", argv[0]);

        broker.shutdown();
    }

    /// The names an agent's tool schema carries, built exactly as a
    /// `chatty-tui` child would build it after `apply_tool_overrides` has
    /// applied `--disable <groups>`.
    async fn tool_names_after(disable: &[&str]) -> Vec<String> {
        // Resolves repository paths for the always-on `list_mcp` tool; a
        // no-op after the first call.
        let _ = chatty_core::init_repositories();
        let workspace = tempfile::tempdir().expect("a workspace");

        let mut settings = ExecutionSettingsModel {
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            ..ExecutionSettingsModel::default()
        };
        let disable: Vec<String> = disable.iter().map(|s| s.to_string()).collect();
        crate::apply_tool_overrides(&mut settings, &[], &disable)
            .expect("test-only disable list names valid groups");

        let ctx = AgentBuildContext {
            pending_approvals: Some(ExecutionApprovalStore::new().get_pending_approvals()),
            pending_clarifications: Some(ClarificationStore::new().get_pending_clarifications()),
            pending_write_approvals: Some(WriteApprovalStore::new().get_pending_approvals()),
            ..AgentBuildContext::from_services(AgentServices {
                exec_settings: Some(settings),
                ..AgentServices::default()
            })
        };
        // Ollama: its client is built without network access or credentials.
        let built = AgentClient::from_model_config_with_tools(
            &ModelConfig::new(
                "gemma4:26b".to_string(),
                "gemma4:26b".to_string(),
                ProviderType::Ollama,
                "gemma4:26b".to_string(),
            ),
            &ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
            ctx,
        )
        .await
        .expect("the agent builds without network access");

        built
            .client
            .agent
            .tool_definitions(None)
            .await
            .expect("tool definitions resolve")
            .into_iter()
            .map(|definition| definition.name)
            .collect()
    }

    /// The issue's "Verify", last clause: a reviewer child — `--disable
    /// fs-write,shell,git` — has no `write_file` in its schema, while a
    /// coder child, with nothing disabled, does.
    #[tokio::test]
    async fn a_reviewer_child_has_no_write_file_tool_in_its_schema() {
        let coder = tool_names_after(&[]).await;
        assert!(
            coder.iter().any(|name| name == "write_file"),
            "the coder keeps write_file, so the reviewer check is not vacuous: {coder:?}"
        );

        let reviewer = tool_names_after(&REVIEWER_DISABLED).await;
        assert!(
            !reviewer.iter().any(|name| name == "write_file"),
            "the reviewer must not be able to edit: {reviewer:?}"
        );
        assert!(
            reviewer.iter().any(|name| name == "read_file"),
            "the reviewer still reads: {reviewer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Declared roles: a preamble and a tool profile (ADR-0011 C11 / AGE-405)
// ---------------------------------------------------------------------------

mod declared_roles {
    //! AGE-405's verification: a `local-reviewer` declared with
    //! `tools: "reviewer"` and a preamble, taken through the whole chain it
    //! travels in production — `resolve_virtual_agents` turns the
    //! declaration into argv, `chatty-tui`'s own parser reads that argv back,
    //! and the agent it builds from it is the one checked.
    //!
    //! What is checked is what the issue asks for: no `write_file` in the
    //! schema, `shell_execute` in it (a reviewer runs the tests), the
    //! preamble in the request's first system message, and the profile on the
    //! card `list_agents` returns.

    use std::path::PathBuf;
    use std::sync::Arc;

    use chatty_core::factories::{AgentBuildContext, AgentClient, AgentServices};
    use chatty_core::models::clarification_store::ClarificationStore;
    use chatty_core::models::execution_approval_store::ExecutionApprovalStore;
    use chatty_core::models::write_approval_store::WriteApprovalStore;
    use chatty_core::services::virtual_agents::resolve_virtual_agents;
    use chatty_core::settings::models::models_store::ModelConfig;
    use chatty_core::settings::models::module_settings::VirtualAgentConfig;
    use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
    use chatty_core::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};
    use chatty_core::tools::list_agents_tool::{ListAgentsTool, ListAgentsToolArgs};
    use clap::Parser;
    use parking_lot::Mutex;
    use rig_agent::completion::Prompt;
    use rig_agent::tool::Tool;

    use crate::participant::broker::Broker;

    const REVIEWER: &str = "local-reviewer";
    const PREAMBLE: &str = "You are the reviewer on this team. Read the diff, run the tests, \
                            and report what you found; never edit the tree yourself.";

    /// The team the issue's manual run declares: one reviewer, with a role.
    fn reviewer_team() -> ModuleSettingsModel {
        ModuleSettingsModel {
            virtual_agents: vec![VirtualAgentConfig {
                name: REVIEWER.to_string(),
                model: Some("gemma4:26b".to_string()),
                tools: Some("reviewer".to_string()),
                preamble: Some(PREAMBLE.to_string()),
                ..VirtualAgentConfig::default()
            }],
            ..ModuleSettingsModel::default()
        }
    }

    /// The reviewer's argv, exactly as the broker would spawn its children
    /// with, read back through `chatty-tui`'s own parser.
    fn reviewer_role() -> chatty_core::factories::AgentRole {
        let specs = resolve_virtual_agents(&[], &[], &reviewer_team(), &[]);
        let argv = ["chatty-tui".to_string()]
            .into_iter()
            .chain(specs[0].args.iter().cloned());
        let cli = crate::Cli::try_parse_from(argv).expect("the worker's argv parses");
        assert_eq!(cli.tools.as_deref(), Some("reviewer"));
        crate::resolve_role(cli.tools.as_deref(), cli.preamble.as_deref())
            .expect("the declared profile exists")
    }

    /// A daemon that answers `POST /api/chat` with a failure and keeps the
    /// request body, so the test can read the messages rig actually sent.
    /// The turn is not what is under test here; the system message is.
    async fn recording_ollama() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
        let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = bodies.clone();
        let app = axum::Router::new().fallback(move |body: String| {
            let sink = sink.clone();
            async move {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    sink.lock().push(json);
                }
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "no model here",
                )
            }
        });
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(tcp, app).await.ok();
        });
        (format!("http://127.0.0.1:{port}"), bodies)
    }

    /// Build the agent a reviewer child builds: its role, and execution
    /// settings with the shell on, since a reviewer's job is to run tests.
    async fn build_reviewer_agent(
        base_url: String,
    ) -> chatty_core::factories::agent_factory::BuiltAgent {
        // Resolves repository paths for the always-on `list_mcp` tool; a
        // no-op after the first call.
        let _ = chatty_core::init_repositories();
        let workspace = tempfile::tempdir().expect("a workspace");

        let settings = ExecutionSettingsModel {
            enabled: true,
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            ..ExecutionSettingsModel::default()
        };
        let ctx = AgentBuildContext {
            role: reviewer_role(),
            pending_approvals: Some(ExecutionApprovalStore::new().get_pending_approvals()),
            pending_clarifications: Some(ClarificationStore::new().get_pending_clarifications()),
            pending_write_approvals: Some(WriteApprovalStore::new().get_pending_approvals()),
            ..AgentBuildContext::from_services(AgentServices {
                exec_settings: Some(settings),
                ..AgentServices::default()
            })
        };
        let mut provider = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        provider.base_url = Some(base_url);
        AgentClient::from_model_config_with_tools(
            &ModelConfig::new(
                "gemma4:26b".to_string(),
                "gemma4:26b".to_string(),
                ProviderType::Ollama,
                "gemma4:26b".to_string(),
            ),
            &provider,
            ctx,
        )
        .await
        .expect("the reviewer agent builds without network access")
    }

    /// The issue's "Verify", first three clauses: no `write_file`,
    /// `shell_execute` present, and the preamble in the first system message.
    #[tokio::test]
    async fn a_declared_reviewer_has_its_profiles_tools_and_its_preamble() {
        let (base_url, bodies) = recording_ollama().await;
        let built = build_reviewer_agent(base_url).await;

        let names: Vec<String> = built
            .client
            .agent
            .tool_definitions(None)
            .await
            .expect("tool definitions resolve")
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        assert!(
            !names.iter().any(|name| name == "write_file"),
            "a reviewer must not be able to edit: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "shell_execute"),
            "a reviewer runs the tests: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "read_file"),
            "a reviewer still reads: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "ask_user"),
            "a profiled worker keeps the input-required chain to its leader \
             (AGE-306): {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "invoke_agent"),
            "the profile is an allowlist, and it does not name the agent tools: {names:?}"
        );

        // The profile is also what makes this worth doing: 53 schemas is what
        // a reviewer used to carry.
        assert!(
            names.len() < 20,
            "a profiled worker carries a small tool set, got {}: {names:?}",
            names.len()
        );

        let _ = built.client.agent.prompt("review the diff").await;
        let recorded = bodies.lock().clone();
        let body = recorded.first().expect("the agent sent one request");
        let first = &body["messages"][0];
        assert_eq!(first["role"], "system", "{body}");
        let system = first["content"].as_str().unwrap_or_default();
        assert!(
            system.contains(PREAMBLE),
            "the role's standing instructions are missing from the system prompt: {system}"
        );
        assert!(
            system.contains("You run the `reviewer` tool profile"),
            "the prompt names the exact tool set: {system}"
        );
        assert!(
            !system.contains("write_file"),
            "the prompt must not advertise tools the profile removed: {system}"
        );
    }

    /// The issue's "Verify", last clause: the card lists the profile, so the
    /// leader chooses by reading `list_agents`.
    #[tokio::test]
    async fn the_card_lists_the_profile_and_the_roles_first_sentence() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let specs = resolve_virtual_agents(&[], &[], &reviewer_team(), &[]);
        let broker = Broker::start_at(
            dir.path().join("participants.sock"),
            // No child is spawned by `list_agents`; the runner only needs a
            // path it could spawn.
            PathBuf::from("/bin/sh"),
            1,
            specs,
            None,
        )
        .await
        .expect("the broker starts with the declared reviewer");

        let output = ListAgentsTool::new(vec![])
            .with_local_workers([REVIEWER])
            .with_gateway_port(broker.port)
            .call(
                &mut rig_agent::tool::ToolContext::new(),
                ListAgentsToolArgs {},
            )
            .await
            .expect("list_agents succeeds");
        let card = output
            .agents
            .iter()
            .find(|agent| agent.name == REVIEWER)
            .unwrap_or_else(|| panic!("the reviewer is listed, got {:?}", output.agents));

        assert!(
            card.description.contains("Tool profile: reviewer."),
            "{}",
            card.description
        );
        assert!(
            card.description
                .contains("Role: You are the reviewer on this team."),
            "{}",
            card.description
        );

        broker.shutdown();
    }
}

// ---------------------------------------------------------------------------
// The spend cap at the delegation (AGE-416 / ADR-0010)
// ---------------------------------------------------------------------------

mod spend_cap {
    //! AGE-416's verification: a hosted leader built with a `SpendGate`
    //! that refuses gets the typed `cap_exceeded` error from `invoke_agent`
    //! and the broker starts nothing — no child is spawned, no participant
    //! registers. A gate that permits delegates exactly as no gate does; the
    //! no-gate path is every other test in this file, unchanged.
    //!
    //! The refusing case runs against a real `LocalRunner` whose worker is
    //! the argv-recording stand-in: the control shows that, without a gate,
    //! the same delegation *does* spawn it, so "nothing spawned" is a real
    //! finding and not an idle runner.

    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use chatty_core::services::spend_gate::{FixedSpendGate, SpendGate};
    use chatty_core::services::{install_progress_channel, scenarios};
    use chatty_core::session::replay_scenario;
    use chatty_core::tools::LOCAL_AGENT_NAME;
    use chatty_core::tools::invoke_agent_tool::{
        InvokeAgentArgs, InvokeAgentError, InvokeAgentProgress, InvokeAgentTool,
    };
    use chatty_module_registry::ModuleRegistry;
    use chatty_protocol_gateway::ProtocolGateway;
    use chatty_protocol_gateway::participant::{LocalRunner, ParticipantRegistry};
    use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
    use rig_agent::tool::{Tool, ToolContext};
    use tokio::sync::RwLock;

    use super::named_virtual_agents::{spawn_argv_gated_worker, stand_in_binary};
    use super::{NoopProvider, assistant_text, policy, spawn_scripted_worker, start_gateway};

    /// A gateway whose `local-agent` is a real `LocalRunner` spawning the
    /// stand-in binary — the spawn the gate must prevent.
    async fn start_runner_gateway(dir: &Path) -> (u16, ParticipantRegistry) {
        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let modules = Arc::new(RwLock::new(
            ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
        ));
        let gateway = ProtocolGateway::new(modules, 0);
        let registry = gateway.participants();

        let runner = LocalRunner::new(
            stand_in_binary(dir),
            dir.join("participants.sock"),
            registry.clone(),
        )
        .with_agent_name(LOCAL_AGENT_NAME)
        .with_registration_timeout(Duration::from_secs(5));
        let gateway = gateway.with_virtual_agent(Arc::new(runner));

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        let router = gateway.build_router();
        tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        (port, registry)
    }

    /// `invoke_agent` as the factory builds it for a leader with a gate.
    fn leader_tool(port: u16, gate: Option<Arc<dyn SpendGate>>) -> InvokeAgentTool {
        let tool =
            InvokeAgentTool::new(vec![], vec![], Some(port)).with_local_agents([LOCAL_AGENT_NAME]);
        match gate {
            Some(gate) => tool.with_spend_gate(gate),
            None => tool,
        }
    }

    async fn delegate(tool: &InvokeAgentTool) -> Result<String, InvokeAgentError> {
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: LOCAL_AGENT_NAME.to_string(),
                prompt: "do the delegated task".to_string(),
                include_trace: false,
            },
        )
        .await
        .map(|output| output.response)
    }

    /// Over the cap: the typed error, and nothing on the broker side —
    /// no participant in the registry, no child spawned, no progress.
    #[tokio::test]
    async fn a_leader_over_its_cap_is_refused_and_nothing_spawns() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (port, registry) = start_runner_gateway(dir.path()).await;

        let tool = leader_tool(port, Some(Arc::new(FixedSpendGate::refusing(12.5, 10.0))));
        let mut progress_rx = install_progress_channel(&tool.progress_slot());

        let err = delegate(&tool)
            .await
            .expect_err("the delegation is refused");
        assert!(
            matches!(err, InvokeAgentError::CapExceeded(_)),
            "expected the typed cap error, got {err:?}"
        );
        assert_eq!(
            err.to_string(),
            "cap_exceeded: month-to-date $12.50 \u{2265} cap $10.00; no delegation started"
        );

        // Give a spawn that should not have happened every chance to show.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            registry.names().is_empty(),
            "no participant registers: {:?}",
            registry.names()
        );
        assert!(
            !dir.path().join("argv.log").exists(),
            "the runner never spawned a child"
        );
        assert!(
            progress_rx.try_recv().is_err(),
            "nothing started, so the transcript has nothing to render"
        );
    }

    /// The control for the test above: the same runner, no gate, and the
    /// same delegation spawns the stand-in child. Without this, an idle
    /// runner would pass the refusal test for the wrong reason.
    #[tokio::test]
    async fn without_a_gate_the_same_delegation_spawns_a_worker() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (port, registry) = start_runner_gateway(dir.path()).await;
        let events = replay_scenario(
            scenarios()
                .into_iter()
                .find(|s| s.name == "tool_call_then_result")
                .expect("the scenario exists"),
            policy(),
        )
        .await;
        spawn_argv_gated_worker(
            &registry,
            &format!("{LOCAL_AGENT_NAME}-0"),
            dir.path().join("argv.log"),
            events.clone(),
        );

        let response = delegate(&leader_tool(port, None))
            .await
            .expect("the delegation succeeds");

        assert_eq!(response, assistant_text(&events).concat().trim());
        let argv = std::fs::read_to_string(dir.path().join("argv.log"))
            .expect("the runner spawned the stand-in child");
        assert!(
            argv.contains(&format!("--participant-name {LOCAL_AGENT_NAME}-0")),
            "{argv}"
        );
    }

    /// Under the cap: the gate is asked and says yes, and the delegation is
    /// the one every other test in this file makes without a gate.
    #[tokio::test]
    async fn a_leader_under_its_cap_delegates_as_without_a_gate() {
        let events = replay_scenario(
            scenarios()
                .into_iter()
                .find(|s| s.name == "tool_call_then_result")
                .expect("the scenario exists"),
            policy(),
        )
        .await;
        let (port, registry) = start_gateway().await;
        spawn_scripted_worker(&registry, LOCAL_AGENT_NAME, events.clone());

        let tool = leader_tool(port, Some(Arc::new(FixedSpendGate::permitting())));
        let mut progress_rx = install_progress_channel(&tool.progress_slot());

        let response = delegate(&tool).await.expect("the delegation succeeds");

        assert_eq!(response, assistant_text(&events).concat().trim());
        let mut started = false;
        while let Ok(event) = progress_rx.try_recv() {
            started |= matches!(event, InvokeAgentProgress::Started { .. });
        }
        assert!(started, "the delegation was started and reported");
    }
}
