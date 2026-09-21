//! Tests for headless-mode helpers (kept separate so the production code
//! file is easier to navigate).

// `super::*` already covers the helper modules: `headless/mod.rs` glob-imports
// `answer_file`, `recovery` and `tool_format`, and a child module sees its
// parent's bindings. Naming them again here was redundant.
use super::*;

use chatty_core::models::message_types::{ExecutionEngine, ToolSource};
use std::collections::BTreeSet;

use crate::engine::{ToolCallInfo, ToolCallState};

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

/// AGE-401: a delegation is logged like any other tool, input at the start
/// and output at the end, so a chain can be audited from the leader's log.
#[test]
fn logs_a_delegation_with_its_input_and_output() {
    use chatty_core::tools::invoke_agent_tool::InvokeAgentProgress;

    let mut agent = None;
    let started = format_delegation_lines(
        &InvokeAgentProgress::Started {
            agent_name: "local-coder".to_string(),
            prompt: "Fix the overdraft bug.".to_string(),
            source: ToolSource::Local,
        },
        &mut agent,
    );
    assert_eq!(
        started,
        vec![
            String::new(),
            "  [agent: local-coder] [local] ⟳ running".to_string(),
            "    input".to_string(),
            "      Fix the overdraft bug.".to_string(),
        ]
    );
    assert!(
        format_delegation_lines(&InvokeAgentProgress::Text("✓ read_file".into()), &mut agent)
            .is_empty(),
        "the worker's own progress is its stderr's, not repeated here"
    );
    let finished = format_delegation_lines(
        &InvokeAgentProgress::Finished {
            success: false,
            result: Some("⚠️ Agent 'local-coder' reported failure".to_string()),
            usage: None,
        },
        &mut agent,
    );
    assert_eq!(
        finished,
        vec![
            String::new(),
            "  [agent: local-coder] ✗ failed".to_string(),
            "    error".to_string(),
            "      ⚠️ Agent 'local-coder' reported failure".to_string(),
        ]
    );
    assert!(agent.is_none(), "the finish line consumes the name");
}

/// AGE-136: the headless trace shows the approval and the plan as their own
/// blocks, not only tool headers.
#[test]
fn formats_the_approval_block_and_its_verdict() {
    let approval = crate::engine::ApprovalInfo {
        id: "a1".to_string(),
        command: "rm -rf build".to_string(),
        is_sandboxed: false,
        decision: None,
    };
    assert_eq!(
        format_approval_requested(&approval),
        "  ? Approve [host] rm -rf build"
    );
    assert_eq!(format_approval_resolved(true), "  ✓ allowed");
    assert_eq!(format_approval_resolved(false), "  ✗ denied");
}

#[test]
fn formats_the_plan_card_after_a_todo_result() {
    let snapshot: chatty_core::services::AgentTaskSnapshot = serde_json::from_str(
        r#"{"goal":"ship","todos":[{"id":"t1","title":"Read it","description":"","status":"done"},{"id":"t2","title":"Write it","description":"","status":"in_progress"}],"write_todos_called":true,"verified":false,"evidence":[]}"#,
    )
    .unwrap();
    let lines = format_plan_lines(&snapshot);
    assert!(lines[0].starts_with("  ▣ Plan"), "{lines:?}");
    assert!(lines[0].ends_with("1/2"), "{lines:?}");
    assert!(lines[1].contains("Read it"), "{lines:?}");
    assert!(lines[2].contains("Write it"), "{lines:?}");
    assert_eq!(lines.len(), 3);
}

#[test]
fn keeps_plain_text_payload_lines() {
    assert_eq!(
        tool_payload_lines("stdout line 1\nstderr line 2\n"),
        vec!["stdout line 1".to_string(), "stderr line 2".to_string(),]
    );
}

#[test]
fn detects_answer_file_requirement() {
    assert!(prompt_requires_answer_file(
        "write ONLY the final answer to `/app/answer.txt`"
    ));
    assert!(prompt_requires_answer_file(
        "Create ANSWER.TXT once you are done"
    ));
    assert!(!prompt_requires_answer_file(
        "Explain the result in the terminal"
    ));
}

#[test]
fn tool_budget_stop_only_applies_to_answer_file_tasks() {
    assert!(!prompt_requires_answer_file("Explain the result"));
    assert_eq!(MAX_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION, 16);
}

#[test]
fn normalizes_acronym_letter_answer_from_prompt() {
    let prompt = r#"What does "R" stand for in the three core policies?"#;
    assert_eq!(
        normalize_answer_for_prompt("original research", prompt),
        "research"
    );
    assert_eq!(
        normalize_answer_for_prompt("No original research", prompt),
        "research"
    );
}

#[test]
fn does_not_normalize_research_without_letter_prompt() {
    assert_eq!(
        normalize_answer_for_prompt("original research", "Which policy was violated?"),
        "original research"
    );
}

#[test]
fn strips_stray_sentence_period_from_short_answer_file_answers() {
    let prompt = "Write ONLY the final answer to `/app/answer.txt`. The answer should be a single, short string.";
    assert_eq!(
        normalize_answer_for_prompt("Extremely.", prompt),
        "Extremely"
    );
    assert_eq!(normalize_answer_for_prompt("U.S.", prompt), "U.S.");
    assert_eq!(normalize_answer_for_prompt("3.14", prompt), "3.14");
}

#[test]
fn extracts_labeled_answer_candidate() {
    assert_eq!(
        candidate_from_line(
            "Best: NexPay = 0.63",
            "Write answer in scheme:fee format to /app/answer.txt",
            true
        )
        .as_deref(),
        Some("NexPay:0.63")
    );
    assert_eq!(
        candidate_from_line("Final answer: Not Applicable", "write answer.txt", true).as_deref(),
        Some("Not Applicable")
    );
}

#[test]
fn rejects_verbose_or_error_candidates() {
    assert!(candidate_from_line("Error: something failed", "answer.txt", false).is_none());
    assert!(
        candidate_from_line(
            "Best: this is a long explanatory sentence with far too many words to be a scalar",
            "answer.txt",
            true
        )
        .is_none()
    );
    assert!(
            candidate_from_line(
                "Best: Practices for Choosing an ACI",
                "Answer must be just the selected card scheme and the associated cost rounded to 2 decimals in this format: {card_scheme}:{fee}",
                true
            )
            .is_none()
        );
    assert!(
        candidate_from_line(
            "Merchant characteritics include",
            "Answer must be just the fee rounded to 2 decimals.",
            false
        )
        .is_none()
    );
    assert_eq!(
        candidate_from_line(
            "0.0",
            "Answer must be just the fee rounded to 2 decimals.",
            false
        )
        .as_deref(),
        Some("0.0")
    );
}

#[test]
fn answer_fallback_only_uses_candidate_source_tools() {
    assert!(is_candidate_source_tool("execute_code"));
    assert!(is_candidate_source_tool("query_data"));
    assert!(!is_candidate_source_tool("doc_retriever"));
    assert!(!is_candidate_source_tool("read_file"));
}

#[test]
fn inferred_answer_fallback_is_opt_in() {
    // The default must be conservative because unverified candidates can be
    // worse than a missing answer file. Strictly formatted answer-file tasks
    // are safe enough to salvage because candidates must match the format.
    assert!(!prompt_has_strict_answer_format("write answer.txt"));
    assert!(prompt_has_strict_answer_format(
        "Answer must be just the selected card scheme and the associated cost rounded to 2 decimals in this format: {card_scheme}:{fee}"
    ));
    assert!(candidate_matches_prompt_format(
        "NexPay:0.63",
        "format: {card_scheme}:{fee}"
    ));
    assert!(!candidate_matches_prompt_format(
        "Practices for Choosing an ACI",
        "format: {card_scheme}:{fee}"
    ));
}

#[test]
fn extracts_known_paths_for_finalization() {
    let mut paths = BTreeSet::new();
    collect_paths_from_text(
        r#"{"path":"/app/data/payments.csv","note":"see data/merchant_data.json and ./manual.md"}"#,
        &mut paths,
    );
    assert!(paths.contains("/app/data/payments.csv"));
    assert!(paths.contains("data/merchant_data.json"));
}

/// AGE-497: the recovery prompt echoes rig's own `UnknownToolCall` message
/// (which already lists the available/allowed tools) rather than the
/// generic stream-error prompt.
#[test]
fn unknown_tool_call_recovery_prompt_echoes_the_rig_error_and_asks_for_a_real_tool() {
    let rig_message = "UnknownToolCall: model attempted to call unknown or disallowed tool \
                        `web_search`. Available tools: [\"search_web\"]. Allowed tools for \
                        this turn: [\"search_web\"]";
    let prompt = unknown_tool_call_recovery_prompt(rig_message);
    assert!(prompt.contains("web_search"));
    assert!(prompt.contains("search_web"));
    assert!(prompt.contains("exact name"));
}

/// The headless runner on its own (AGE-196): a turn runs with no engine
/// behind it, and the deferred-send gate `run_headless` relies on holds.
///
/// Driving `run_headless`'s own event loop end-to-end here would need a
/// mocked LLM stream; these pin the runner's contract directly, with a
/// real (network-free) `Conversation` on the session.
mod runner {
    use super::*;
    use crate::engine::{ChatEngineConfig, MessageRole};
    use chatty_core::factories::agent_factory::{AgentBuildContext, AgentServices};
    use chatty_core::settings::models::execution_settings::ExecutionSettingsModel;
    use chatty_core::settings::models::models_store::{ModelConfig, ModelsModel};
    use chatty_core::settings::models::module_settings::ModuleSettingsModel;
    use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
    use std::sync::{Arc, Mutex};

    /// A runner around a real (network-free) `Conversation`. Ollama client
    /// construction is purely local, so this is safe in unit tests. The
    /// agent is built against the session's own store handles, as
    /// `init_conversation` does.
    async fn test_runner() -> (HeadlessRunner, mpsc::UnboundedReceiver<AppEvent>) {
        test_runner_with_team(None).await
    }

    async fn test_runner_with_team(
        team: Option<chatty_core::services::team::Team>,
    ) -> (HeadlessRunner, mpsc::UnboundedReceiver<AppEvent>) {
        let _ = chatty_core::init_repositories();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let model_config = ModelConfig::new(
            "m1".to_string(),
            "Test Model".to_string(),
            ProviderType::Ollama,
            "llama3.2".to_string(),
        );
        let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
        let mut runner = HeadlessRunner::new(
            ChatEngineConfig {
                model_config: model_config.clone(),
                provider_config: provider_config.clone(),
                execution_settings: ExecutionSettingsModel::default(),
                module_settings: ModuleSettingsModel::default(),
                broker_port: None,
                models: ModelsModel::default(),
                providers: Vec::new(),
                mcp_service: None,
                memory_service: None,
                search_settings: None,
                embedding_service: None,
                user_secrets: Vec::new(),
                remote_agents: Vec::new(),
                module_agents: Vec::new(),
                role: Default::default(),
                team,
                is_sub_agent: true,
                services_loaded: true,
                surface: chatty_core::services::StreamSurface::Headless,
            },
            event_tx,
        );

        runner
            .session
            .create_conversation(
                "c1".to_string(),
                "New Chat".to_string(),
                &model_config,
                &provider_config,
                AgentBuildContext::from_services(AgentServices::default()),
            )
            .await
            .expect("conversation should build without network access");
        runner.is_ready = true;
        (runner, event_rx)
    }

    /// AGE-196 acceptance: a headless turn runs on the session directly —
    /// no `ChatEngine`, no terminal state — and every event of it reaches
    /// the parent that delegated it, which since ADR-0011's C4 means the
    /// event observer a broker participant installs.
    #[tokio::test]
    async fn a_headless_turn_runs_on_the_session_and_reports_to_the_parent() {
        let (mut runner, mut event_rx) = test_runner().await;
        let observed: Arc<Mutex<Vec<chatty_core::session::SessionEvent>>> = Arc::default();
        let sink = observed.clone();
        runner.set_event_observer(Arc::new(move |event| {
            sink.lock().unwrap().push(event.clone())
        }));

        let input = runner
            .prepare_send("what is this?".to_string(), true)
            .expect("runner is ready and idle");
        let scenario = chatty_core::services::scenarios()
            .into_iter()
            .find(|s| s.name == "tool_call_then_result")
            .expect("scenario exists");
        let turn = runner
            .session
            .begin_scripted_turn(input, scenario, runner.event_sink())
            .expect("turn starts");
        assert!(runner.is_streaming);
        turn.await;
        while let Ok(event) = event_rx.try_recv() {
            runner.handle_event(event);
        }

        assert!(!runner.is_streaming);
        let last = runner.transcript.messages.last().expect("assistant row");
        assert!(matches!(last.role, MessageRole::Assistant));
        assert_eq!(last.text(), "It is the readme.");
        assert!(runner.transcript.tool_call("call-1").is_some());
        assert_eq!(runner.session.conversation().unwrap().messages().len(), 2);

        let events = observed.lock().unwrap();
        assert!(matches!(
            events.first(),
            Some(chatty_core::session::SessionEvent::TurnStarted)
        ));
        assert!(matches!(
            events.last(),
            Some(chatty_core::session::SessionEvent::TurnEnded)
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, chatty_core::session::SessionEvent::ToolCallResult { id, .. } if id == "call-1"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, chatty_core::session::SessionEvent::Text(_))),
            "the observer sees the answer too — the broker maps it to artifact              chunks, which is what lets a parent stream a delegated answer"
        );
    }

    /// AGE-407: a `--team` leader's first *human* turn opens with
    /// "read_skill <skill> and follow it"; the next one does not, and a
    /// protocol follow-up never does — one arriving before the human turn
    /// neither carries the instruction nor consumes it.
    #[tokio::test]
    async fn a_team_leaders_first_human_turn_opens_with_the_skill_instruction() {
        let team = chatty_core::services::team::load_team("coder-reviewer", None, None)
            .expect("the preset loads");
        let (mut runner, _event_rx) = test_runner_with_team(Some(team)).await;
        let text = |input: &chatty_core::session::TurnInput| -> String {
            input
                .contents
                .iter()
                .filter_map(|c| match c {
                    rig_core::message::UserContent::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect()
        };

        let follow_up = runner
            .prepare_send(
                "Agent protocol follow-up: call verify_completion.".to_string(),
                false,
            )
            .expect("runner is ready and idle");
        assert_eq!(
            follow_up.kind,
            chatty_core::session::TurnKind::ProtocolFollowUp
        );
        assert_eq!(
            text(&follow_up),
            "Agent protocol follow-up: call verify_completion.",
            "a follow-up never carries the instruction"
        );

        runner.is_streaming = false;
        let first = runner
            .prepare_send("Fix the overdraft bug.".to_string(), true)
            .expect("runner is idle again");
        assert_eq!(first.kind, chatty_core::session::TurnKind::Human);
        assert_eq!(
            text(&first),
            "read_skill coder-reviewer and follow it.\n\nFix the overdraft bug."
        );
        let user_row = runner
            .transcript
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .expect("the user row");
        assert_eq!(
            user_row.text(),
            text(&first),
            "the transcript shows what the model was asked"
        );

        runner.is_streaming = false;
        let second = runner
            .prepare_send("And the tests?".to_string(), true)
            .expect("runner is idle again");
        assert_eq!(text(&second), "And the tests?");
    }

    /// AGE-441: a `--team` leader shares the workspace with its workers, so
    /// the task's answer file appearing on disk is a worker's result — the
    /// leader's turn goes on to whatever its protocol says comes next (a
    /// second delegation, a REVISE loop) instead of stopping on the spot as a
    /// lone agent would.
    #[tokio::test]
    async fn a_team_leader_keeps_going_when_a_worker_writes_the_answer_file() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let team = chatty_core::services::team::load_team("coder-reviewer", None, None)
            .expect("the preset loads");
        let (mut leader, _event_rx) = test_runner_with_team(Some(team)).await;
        leader.execution_settings.workspace_dir =
            Some(workspace.path().to_string_lossy().into_owned());
        assert!(
            leader.is_team_leader(),
            "the role comes from the team config"
        );
        assert!(!answer_file_exists(&leader));
        assert!(!stops_on_answer_file(&leader, true));

        // The first worker's `final_answer` lands in the shared workspace
        // while the leader is still waiting on its `invoke_agent` result.
        std::fs::write(workspace.path().join("answer.txt"), "42").unwrap();
        assert!(answer_file_exists(&leader));
        assert!(
            !stops_on_answer_file(&leader, true),
            "a worker's answer file must not end the leader's turn"
        );
    }

    /// AGE-441 regression: a lone `--headless` agent (and a worker, which
    /// runs without `--team`) still stops the moment the task's answer file
    /// exists after one of its own tool calls — and only on tasks that use
    /// the answer-file convention at all.
    #[tokio::test]
    async fn a_lone_agent_still_stops_once_its_answer_file_exists() {
        let workspace = tempfile::tempdir().expect("a workspace");
        let (mut lone, _event_rx) = test_runner().await;
        lone.execution_settings.workspace_dir =
            Some(workspace.path().to_string_lossy().into_owned());
        assert!(!lone.is_team_leader());
        assert!(!stops_on_answer_file(&lone, true), "no file yet");

        std::fs::write(workspace.path().join("answer.txt"), "42").unwrap();
        assert!(stops_on_answer_file(&lone, true));
        assert!(
            !stops_on_answer_file(&lone, false),
            "a task that never asked for an answer file is not stopped by one"
        );
    }

    /// AGE-452: a `--team` leader has no `event_observer` of its own —
    /// nothing upstream to relay a clarification to, whether the question is
    /// the leader's own `ask_user` call or one AGE-306 relayed up from a
    /// delegated worker's. `cancel_all()` would hard-fail whichever turn
    /// asked it (the "Clarification cancelled" error), so the leader must
    /// answer it with a default instead. This spawns a real
    /// `request_clarification` call against the leader's own store — the
    /// same call a worker's relayed question or the leader's own `ask_user`
    /// tool makes — and drives the resulting event through `handle_event`
    /// exactly as `run_headless` would.
    #[tokio::test]
    async fn a_team_leaders_clarification_is_answered_not_cancelled() {
        let team = chatty_core::services::team::load_team("coder-reviewer", None, None)
            .expect("the preset loads");
        let (mut leader, _event_rx) = test_runner_with_team(Some(team)).await;
        assert!(leader.is_team_leader());

        let pending = leader.session.approval_handles().pending_clarifications;
        let question = chatty_core::models::clarification_store::ClarifyingQuestion {
            id: "q1".to_string(),
            question: "Which database?".to_string(),
            options: vec![],
        };
        let waiter = tokio::spawn({
            let pending = pending.clone();
            let question = question.clone();
            async move {
                chatty_core::models::clarification_store::request_clarification(
                    &pending,
                    vec![question],
                )
                .await
            }
        });

        // Learned the way a unit test outside chatty-core has to: poll the
        // store rather than install a notifier (only `AgentSession`'s own
        // turn machinery can do that — see `ClarificationStore::pending_ids`).
        let id = loop {
            if let Some(id) = leader
                .session
                .clarifications()
                .pending_ids()
                .into_iter()
                .next()
            {
                break id;
            }
            tokio::task::yield_now().await;
        };

        leader.handle_event(AppEvent::ClarificationRequested {
            id,
            questions: vec![question],
        });

        let answers = waiter
            .await
            .unwrap()
            .expect("a --team leader must answer a clarification, not cancel it");
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].id, "q1");
        assert!(
            answers[0].custom,
            "a canned default is not one of the question's pre-made options"
        );
    }

    /// AGE-452 regression: a lone `--headless` agent (no `--team`, no
    /// parent) keeps the pre-existing behavior — nobody can answer it, so
    /// the tool is unblocked with an error rather than a canned default.
    #[tokio::test]
    async fn a_lone_agents_clarification_is_still_cancelled() {
        let (mut lone, _event_rx) = test_runner().await;
        assert!(!lone.is_team_leader());

        let pending = lone.session.approval_handles().pending_clarifications;
        let question = chatty_core::models::clarification_store::ClarifyingQuestion {
            id: "q1".to_string(),
            question: "Which database?".to_string(),
            options: vec![],
        };
        let waiter = tokio::spawn({
            let pending = pending.clone();
            async move {
                chatty_core::models::clarification_store::request_clarification(
                    &pending,
                    vec![question],
                )
                .await
            }
        });

        let id = loop {
            if let Some(id) = lone
                .session
                .clarifications()
                .pending_ids()
                .into_iter()
                .next()
            {
                break id;
            }
            tokio::task::yield_now().await;
        };

        lone.handle_event(AppEvent::ClarificationRequested {
            id,
            questions: vec![
                chatty_core::models::clarification_store::ClarifyingQuestion {
                    id: "q1".to_string(),
                    question: "Which database?".to_string(),
                    options: vec![],
                },
            ],
        });

        let err = waiter.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("cancelled"), "got: {err}");
    }

    /// T3/AGE-242: `stop_stream()` only sets the cancel flag, so a
    /// `send_message()` right after it is refused; the deferred-send pattern
    /// in `run_headless` holds the prompt until the cancellation completes.
    #[tokio::test]
    async fn send_message_right_after_stop_stream_is_refused_but_succeeds_once_cancelled() {
        let (mut runner, _event_rx) = test_runner().await;

        runner.send_message("first turn".to_string());
        assert!(runner.is_streaming);
        let messages_after_first_send = runner.transcript.messages.len();

        runner.stop_stream();
        runner.send_message("queued pivot prompt".to_string());
        assert_eq!(
            runner.transcript.messages.len(),
            messages_after_first_send,
            "send_message must no-op while is_streaming is still true"
        );

        runner.handle_event(AppEvent::StreamCancelled);
        runner.handle_event(AppEvent::StreamCompleted);
        assert!(!runner.is_streaming);

        runner.send_message("queued pivot prompt".to_string());
        assert!(
            runner.transcript.messages.len() > messages_after_first_send,
            "send_message must succeed once the cancellation has completed"
        );
    }

    /// AGE-503 regression: `max_agent_turns` is rig's per-`stream_prompt`-call
    /// budget (a fresh `AgentRun` per call, `current_turn` starting at 0
    /// every time) — not a cumulative total across the run. Finalization
    /// used to narrow that per-call budget down to an absolute
    /// `FINALIZATION_MAX_AGENT_TURNS` (12) via `.min()`, even when the
    /// operator had configured a larger one (e.g. `--max-agent-turns 30`),
    /// so a wrap-up prompt that legitimately needed more than 12 model
    /// calls died with `MaxTurnsError`. A configured budget already at or
    /// above the floor must be left untouched.
    #[tokio::test]
    async fn finalization_never_narrows_a_configured_budget_below_the_floor() {
        let (mut runner, _event_rx) = test_runner().await;
        runner.execution_settings.max_agent_turns = 30;
        for _ in 0..20 {
            runner.transcript.start_assistant();
        }

        send_answer_file_finalization_prompt(
            &mut runner,
            "Write ONLY the final answer to /app/answer.txt",
        );

        assert_eq!(
            runner.execution_settings.max_agent_turns, 30,
            "a configured budget already above the floor must be left untouched \
             (the old `.min()` code would have narrowed this to 12)"
        );
    }

    /// AGE-503: there is no cumulative "turns used" accounting anywhere in
    /// the real turn-limit path, so the finalization budget must not depend
    /// on how many assistant rows the transcript already has. A runner with
    /// 0 prior rows and one with 20 must land on the exact same budget.
    #[tokio::test]
    async fn finalization_budget_is_independent_of_turns_already_used() {
        let (mut fresh, _event_rx) = test_runner().await;
        fresh.execution_settings.max_agent_turns = 30;
        send_answer_file_finalization_prompt(
            &mut fresh,
            "Write ONLY the final answer to /app/answer.txt",
        );

        let (mut used, _event_rx2) = test_runner().await;
        used.execution_settings.max_agent_turns = 30;
        for _ in 0..20 {
            used.transcript.start_assistant();
        }
        send_answer_file_finalization_prompt(
            &mut used,
            "Write ONLY the final answer to /app/answer.txt",
        );

        assert_eq!(
            fresh.execution_settings.max_agent_turns, used.execution_settings.max_agent_turns,
            "the finalization budget must be identical regardless of turns already used"
        );
    }

    /// AGE-503: a configured budget smaller than `FINALIZATION_MAX_AGENT_TURNS`
    /// is raised to the floor so the wrap-up pass always gets at least 12
    /// model calls.
    #[tokio::test]
    async fn finalization_raises_a_too_small_configured_budget_to_the_floor() {
        let (mut runner, _event_rx) = test_runner().await;
        runner.execution_settings.max_agent_turns = 5;

        send_answer_file_finalization_prompt(
            &mut runner,
            "Write ONLY the final answer to /app/answer.txt",
        );

        assert_eq!(
            runner.execution_settings.max_agent_turns,
            FINALIZATION_MAX_AGENT_TURNS
        );
    }
}
