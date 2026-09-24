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
    assert!(prompt_requires_answer_file(&[
        "write ONLY the final answer to `/app/answer.txt`",
        ""
    ]));
    assert!(prompt_requires_answer_file(&[
        "Create ANSWER.TXT once you are done",
        ""
    ]));
    assert!(!prompt_requires_answer_file(&[
        "Explain the result in the terminal",
        ""
    ]));
}

#[test]
fn detects_answer_file_requirement_from_preamble_only() {
    // AGE evidence: the instruction to write /app/answer.txt sometimes
    // arrives via --preamble rather than --message; detection must look at
    // both instead of only the message.
    assert!(prompt_requires_answer_file(&[
        "What is the total revenue?",
        "You are FinanceAgent. Write your final answer to /app/answer.txt."
    ]));
    assert!(!prompt_requires_answer_file(&[
        "What is the total revenue?",
        "You are FinanceAgent. Be concise."
    ]));
}

#[test]
fn tool_budget_stop_only_applies_to_answer_file_tasks() {
    assert!(!prompt_requires_answer_file(&["Explain the result", ""]));
}

/// The exploration budget follows the run's turn cap: GAIA runs at 50
/// turns were cut at 16 tool results while still exploring (one needed ~27
/// calls to reach its API answer).
#[test]
fn answer_file_tool_budget_scales_with_the_turn_cap() {
    assert_eq!(answer_file_tool_budget(50), 40, "80 % of a 50-turn cap");
    assert_eq!(answer_file_tool_budget(30), 24);
    assert_eq!(answer_file_tool_budget(51), 41, "rounds up");
    assert_eq!(
        answer_file_tool_budget(10),
        MIN_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION,
        "a small cap ends through TurnBudget first; the budget keeps its floor"
    );
    assert_eq!(
        answer_file_tool_budget(0),
        UNCAPPED_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION
    );
    const {
        assert!(UNCAPPED_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION > 16);
        // Stops after TurnBudget's 75 % wrap-up note, not before it.
        assert!(ANSWER_FILE_TOOL_BUDGET_PERCENT > 75);
    }
}

fn failed_tool(name: &str, output: &str) -> ToolCallInfo {
    ToolCallInfo {
        id: "t1".to_string(),
        name: name.to_string(),
        input: "{}".to_string(),
        output: Some(output.to_string()),
        state: ToolCallState::Error,
        source: ToolSource::Local,
        execution_engine: None,
    }
}

/// A single `read_file` outside the workspace used to count toward the
/// 3-failure budget and trip finalization; the harness's own policy saying
/// no is not the model being stuck.
#[test]
fn sandbox_and_path_policy_refusals_are_not_counted_as_failures() {
    let refused = failed_tool(
        "read_file",
        "Error: read_file: Access denied: path '/etc/passwd' is outside the workspace root",
    );
    assert!(tool_result_looks_failed(&refused));
    assert!(tool_result_is_policy_refusal(&refused));

    let crashed = failed_tool("shell_execute", "Traceback (most recent call last): ...");
    assert!(tool_result_looks_failed(&crashed));
    assert!(!tool_result_is_policy_refusal(&crashed));
    const { assert!(MAX_FAILED_TOOL_RESULTS_BEFORE_FINALIZATION > 3) };
}

/// "not allowed" is ordinary program and page text; matching it hid real
/// failures from the failure budget. Only the harness's own refusal of a
/// non-command tool is exempt.
#[test]
fn program_output_that_says_not_allowed_is_still_a_failure() {
    for (name, output) in [
        (
            "execute_code",
            "Traceback (most recent call last):\nValueError: negative dimensions are not allowed",
        ),
        (
            "shell_execute",
            "{\"exit_code\": 22, \"stdout\": \"405 Method Not Allowed\"}",
        ),
        (
            "shell_execute",
            "{\"exit_code\": 1, \"stderr\": \"Access denied: path '/x' is outside the workspace root\"}",
        ),
        ("fetch", "Error: fetch: HTTP 405 Method Not Allowed"),
    ] {
        let tc = failed_tool(name, output);
        assert!(tool_result_looks_failed(&tc), "{name}: {output}");
        assert!(!tool_result_is_policy_refusal(&tc), "{name}: {output}");
    }
    for refusal in [
        "Error: write_file: Output path '/etc/x' is outside the workspace directory",
        "Error: list_directory: Access denied: glob pattern '/**' is outside the workspace root",
        "Error: query_data: Path not allowed: /etc/data.csv",
    ] {
        assert!(
            tool_result_is_policy_refusal(&failed_tool("read_file", refusal)),
            "{refusal}"
        );
    }
}

/// `python3 count.py; echo -n 5 > /app/answer.txt` wrote 5 while the script
/// printed 7: a command-written answer earns one more model turn, then the
/// next tool result stops the run. Dedicated writes stop at once.
#[test]
fn a_command_written_answer_gets_one_more_turn_and_dedicated_writes_stop_at_once() {
    let is_command = |name: &str| COMMAND_TOOLS.contains(&name);
    assert_eq!(
        answer_file_stop(false, is_command("shell_execute")),
        AnswerFileStop::AfterNextTurn
    );
    assert_eq!(
        answer_file_stop(false, is_command("execute_code")),
        AnswerFileStop::AfterNextTurn
    );
    assert_eq!(
        answer_file_stop(true, is_command("shell_execute")),
        AnswerFileStop::Now,
        "the grace turn is given once; its rewrite then ends the run"
    );
    for dedicated in ["final_answer", "write_file", "apply_diff"] {
        assert_eq!(
            answer_file_stop(false, is_command(dedicated)),
            AnswerFileStop::Now,
            "{dedicated}"
        );
    }
}

#[test]
fn the_finalization_prompt_asks_for_an_answer_from_gathered_evidence() {
    let prompt = build_answer_file_finalization_prompt("Task:\nHow many?", None);
    assert!(prompt.contains("evidence you have gathered"));
    assert!(prompt.contains("one quick check"));
    assert!(!prompt.contains("Do not keep researching"));
    assert!(prompt.contains("How many?"));
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
    use crate::headless::runner::FINAL_PASS_TOOL_TURNS;
    use chatty_core::factories::agent_factory::{AgentBuildContext, AgentServices};
    use chatty_core::services::turn_budget::TurnBudget;
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

    /// `n` model calls that each made one tool call, as the runner sees them.
    fn spend_tool_turns(runner: &mut HeadlessRunner, n: usize) {
        for i in 0..n {
            runner.handle_event(AppEvent::ToolCallStarted {
                id: format!("call_{i}"),
                name: "shell_execute".into(),
            });
            runner.handle_event(AppEvent::ToolCallResult {
                id: format!("call_{i}"),
                result: "ok".into(),
            });
        }
    }

    /// The 76-minute run: `--max-agent-turns 50`, yet 101 shell calls,
    /// because every follow-up pass got a fresh 50. Across the first pass,
    /// stall resumes, pivots and finalizations that each spend all they are
    /// given, the run stays within `max_agent_turns + FINAL_PASS_TOOL_TURNS`.
    #[tokio::test]
    async fn follow_up_passes_share_the_runs_turn_budget() {
        let (mut runner, _event_rx) = test_runner().await;
        runner.execution_settings.max_agent_turns = 50;

        let first = runner.pass_turn_budget(false).unwrap();
        assert_eq!(first, TurnBudget::run_share(50, 50, 0));
        let mut total = 0;
        for final_pass in [false, false, false, true, false, true, true] {
            let budget = runner.pass_turn_budget(final_pass).unwrap();
            if final_pass {
                assert!(budget.tool_turns() <= FINAL_PASS_TOOL_TURNS);
            }
            // The first of these is the first pass: it gets the whole run.
            if total == 0 {
                assert_eq!(budget, first);
            }
            spend_tool_turns(&mut runner, budget.tool_turns());
            total += budget.tool_turns();
        }
        assert_eq!(runner.tool_turns_spent, total);
        assert_eq!(total, 50 + FINAL_PASS_TOOL_TURNS);
        assert_eq!(
            runner.pass_turn_budget(false),
            Some(TurnBudget::run_share(0, 50, 50 + FINAL_PASS_TOOL_TURNS)),
            "a spent run still gets its tool-free last word, and no tools"
        );
    }

    /// A follow-up after a partly spent pass gets what is left; one after a
    /// nearly spent pass still gets the floor, so it can write the answer.
    #[tokio::test]
    async fn a_follow_up_gets_the_rest_of_the_run_or_the_floor() {
        let (mut runner, _event_rx) = test_runner().await;
        runner.execution_settings.max_agent_turns = 50;
        spend_tool_turns(&mut runner, 30);
        assert_eq!(
            runner.pass_turn_budget(false),
            Some(TurnBudget::run_share(20, 50, 30))
        );
        spend_tool_turns(&mut runner, 19);
        assert_eq!(
            runner.pass_turn_budget(false),
            Some(TurnBudget::run_share(FINAL_PASS_TOOL_TURNS, 50, 49))
        );
    }

    /// A parallel batch is one model call, so one tool turn.
    #[tokio::test]
    async fn a_parallel_tool_batch_counts_as_one_turn() {
        let (mut runner, _event_rx) = test_runner().await;
        for id in ["a", "b"] {
            runner.handle_event(AppEvent::ToolCallStarted {
                id: id.into(),
                name: "shell_execute".into(),
            });
        }
        for id in ["a", "b"] {
            runner.handle_event(AppEvent::ToolCallResult {
                id: id.into(),
                result: "ok".into(),
            });
        }
        assert_eq!(runner.tool_turns_spent, 1);
    }

    /// A finalization pass exists to write the answer: it gets the floor,
    /// not a fresh `max_agent_turns` (AGE-503 used to raise it to 12 and
    /// leave every later pass there too).
    #[tokio::test]
    async fn a_finalization_pass_gets_only_the_floor() {
        let (mut runner, mut event_rx) = test_runner().await;
        runner.execution_settings.max_agent_turns = 30;
        runner.scripted_turns = vec![answer_turn("7")].into();

        send_answer_file_finalization_prompt(
            &mut runner,
            "Write ONLY the final answer to /app/answer.txt",
            false,
        );
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        assert_eq!(
            runner.scripted_budgets,
            vec![Some(TurnBudget::run_share(FINAL_PASS_TOOL_TURNS, 30, 0))]
        );
        assert_eq!(runner.execution_settings.max_agent_turns, 30, "untouched");
        assert!(!runner.next_pass_is_final, "only the one pass");
    }

    /// The finalization turn runs on the history the model built, not on a
    /// wiped conversation plus a digest: a history-less pass on GAIA
    /// invented an ID after 16 calls of real exploration were thrown away.
    #[tokio::test]
    async fn finalization_keeps_the_conversation_history() {
        let (mut runner, mut event_rx) = test_runner().await;
        runner.scripted_turns = vec![answer_turn("EXPLORED-EVIDENCE-7"), answer_turn("7")].into();
        runner.send_message("How many? Write ONLY the answer to /app/answer.txt".to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        send_answer_file_finalization_prompt(
            &mut runner,
            "How many? Write ONLY the answer to /app/answer.txt",
            false,
        );
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        let history = format!("{:?}", runner.session.conversation().unwrap().messages());
        assert!(history.contains("EXPLORED-EVIDENCE-7"), "{history}");
        assert!(history.contains("Time to finish"), "{history}");
    }

    /// The tool and failure budgets end the stream mid-turn, and rig hands
    /// back a turn's tool round-trips only with its final response: the cut
    /// turn is in the history as text alone. A finalization that only
    /// pointed at "the evidence gathered above" then had none, so after a
    /// cut the prompt carries the transcript's digest of the tool results.
    #[tokio::test]
    async fn finalization_after_a_cut_turn_carries_the_tool_evidence() {
        let (mut runner, mut event_rx) = test_runner().await;
        runner.scripted_turns = vec![
            Scenario {
                name: "cut_after_a_tool",
                progress: Vec::new(),
                items: vec![
                    ScriptedItem::Chunk(StreamChunk::Text("Querying the API.".into())),
                    ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                        id: "call_1".into(),
                        name: "fetch".into(),
                    }),
                    ScriptedItem::Chunk(StreamChunk::ToolCallInput {
                        id: "call_1".into(),
                        arguments: "{}".into(),
                    }),
                    ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                        id: "call_1".into(),
                        result: "record id UNIQUE-API-ID-42".into(),
                    }),
                    ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
                        StreamErrorKind::Stalled,
                        stalled_stream_message(chatty_core::services::STALL_TIMEOUT),
                    ))),
                ],
            },
            answer_turn("42"),
        ]
        .into();
        let task = "Which id? Write ONLY the answer to /app/answer.txt";
        runner.send_message(task.to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }
        let history = format!("{:?}", runner.session.conversation().unwrap().messages());
        assert!(!history.contains("UNIQUE-API-ID-42"), "{history}");

        send_answer_file_finalization_prompt(&mut runner, task, true);
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        let sent = runner.scripted_inputs.lock().unwrap().clone();
        let finalization = sent.last().expect("finalization prompt sent");
        assert!(finalization.contains("Time to finish"), "{finalization}");
        assert!(finalization.contains("UNIQUE-API-ID-42"), "{finalization}");
        assert!(
            !build_answer_file_finalization_prompt(task, None).contains("digest"),
            "a turn that ended on its own keeps its round-trips; no digest"
        );
    }

    // -------------------------------------------------------------------
    // Stall auto-resume: `run_headless` end to end on scripted turns.
    // -------------------------------------------------------------------

    use chatty_core::services::{
        HEADLESS_STALL_RESUME_ATTEMPTS, Scenario, ScriptedItem, StreamChunk, stalled_stream_message,
    };

    fn stalled_turn() -> Scenario {
        Scenario {
            name: "stalled",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Working on it".into())),
                // What the watchdog hands the handler when it fires.
                ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
                    StreamErrorKind::Stalled,
                    stalled_stream_message(chatty_core::services::STALL_TIMEOUT),
                ))),
            ],
        }
    }

    fn answer_turn(text: &str) -> Scenario {
        Scenario {
            name: "answer",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text(text.to_string())),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        }
    }

    /// A runner that plays `turns` in order, in a scratch workspace (so no
    /// stray answer.txt ends the run), counting the turns that start.
    async fn scripted_runner(
        turns: Vec<Scenario>,
    ) -> (
        HeadlessRunner,
        mpsc::UnboundedReceiver<AppEvent>,
        Arc<Mutex<usize>>,
        tempfile::TempDir,
    ) {
        let (mut runner, event_rx) = test_runner().await;
        let workspace = tempfile::tempdir().unwrap();
        runner.execution_settings.workspace_dir =
            Some(workspace.path().to_string_lossy().into_owned());
        runner.scripted_turns = turns.into();
        let started: Arc<Mutex<usize>> = Arc::default();
        let counter = started.clone();
        runner.set_event_observer(Arc::new(move |event| {
            if matches!(event, chatty_core::session::SessionEvent::TurnStarted) {
                *counter.lock().unwrap() += 1;
            }
        }));
        (runner, event_rx, started, workspace)
    }

    /// A local server that goes quiet for longer than the stall timeout and
    /// then recovers: headless sends the continuation itself and the run
    /// succeeds, instead of exiting non-zero with the work lost.
    #[tokio::test]
    async fn a_stalled_turn_is_resumed_and_the_run_succeeds() {
        let (runner, event_rx, started, _workspace) =
            scripted_runner(vec![stalled_turn(), answer_turn("Resumed and done.")]).await;

        run_headless(runner, event_rx, "summarize the repo".to_string())
            .await
            .expect("the resumed run exits 0");

        assert_eq!(*started.lock().unwrap(), 2, "one stalled turn, one resume");
    }

    /// The resume prompt goes on the same history as a protocol follow-up,
    /// not a human turn, so it cannot refill its own budget.
    #[tokio::test]
    async fn the_resume_prompt_continues_the_same_conversation() {
        let (mut runner, mut event_rx) = test_runner().await;
        runner.scripted_turns = vec![answer_turn("ok")].into();
        let attempts_before = runner.session.recovery_attempts(StreamErrorKind::Stalled);
        let error = StreamError::new(StreamErrorKind::Stalled, "stalled");
        assert!(matches!(
            runner.session.recovery_action(&error),
            RecoveryAction::Retry { .. }
        ));

        runner.send_recovery_prompt(STALL_RESUME_PROMPT.to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        let conversation = runner.session.conversation().unwrap();
        assert!(
            conversation
                .messages()
                .iter()
                .any(|m| format!("{m:?}").contains("interrupted by a stall")),
            "the continuation is part of the history the model sees"
        );
        assert_eq!(
            runner.session.recovery_attempts(StreamErrorKind::Stalled),
            attempts_before + 1,
            "a protocol follow-up keeps the stall budget it spent"
        );
    }

    /// A server that never recovers: the run resumes a bounded number of
    /// times, then fails as before.
    #[tokio::test]
    async fn stall_resumes_are_bounded() {
        let mut turns: Vec<Scenario> = (0..=HEADLESS_STALL_RESUME_ATTEMPTS + 2)
            .map(|_| stalled_turn())
            .collect();
        turns.push(answer_turn("never reached"));
        let (runner, event_rx, started, _workspace) = scripted_runner(turns).await;

        let error = run_headless(runner, event_rx, "summarize the repo".to_string())
            .await
            .expect_err("a server that never recovers still fails the run");

        assert!(error.to_string().contains("stopped responding"), "{error}");
        assert_eq!(
            *started.lock().unwrap(),
            1 + HEADLESS_STALL_RESUME_ATTEMPTS,
            "the first turn plus the bounded resumes"
        );
    }

    /// What the resume prompt tells the model: a stalled turn keeps its
    /// text, but rig hands back its tool round-trips only when a turn
    /// finishes, so they are not in the history the resume runs on.
    #[tokio::test]
    async fn a_stalled_turn_keeps_its_text_but_not_its_tool_results() {
        let (mut runner, mut event_rx) = test_runner().await;
        runner.scripted_turns = vec![Scenario {
            name: "stall_after_a_tool",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Listing files.".into())),
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call_1".into(),
                    name: "list_directory".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallInput {
                    id: "call_1".into(),
                    arguments: "{}".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                    id: "call_1".into(),
                    result: "UNIQUE-TOOL-OUTPUT".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
                    StreamErrorKind::Stalled,
                    stalled_stream_message(chatty_core::services::STALL_TIMEOUT),
                ))),
            ],
        }]
        .into();

        runner.send_message("the task".to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        let history = format!("{:?}", runner.session.conversation().unwrap().messages());
        assert!(history.contains("the task"), "{history}");
        assert!(history.contains("Listing files."), "{history}");
        assert!(!history.contains("UNIQUE-TOOL-OUTPUT"), "{history}");
        assert!(STALL_RESUME_PROMPT.contains("not in the history"));
    }

    /// A stall before the model said anything: the empty turn is rolled
    /// back (AGE-243), taking the task's own message with it, so a
    /// "continue" on that history would ask the model to continue nothing.
    /// The retry re-sends the rolled-back message instead.
    #[tokio::test]
    async fn a_turn_that_stalls_before_any_output_is_retried_with_its_own_message() {
        let silent_stall = Scenario {
            name: "silent_stall",
            progress: Vec::new(),
            items: vec![ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
                StreamErrorKind::Stalled,
                stalled_stream_message(chatty_core::services::STALL_TIMEOUT),
            )))],
        };
        let (runner, event_rx, started, _workspace) =
            scripted_runner(vec![silent_stall, answer_turn("Done.")]).await;
        let sent = runner.scripted_inputs.clone();

        run_headless(runner, event_rx, "Count the files in /data".to_string())
            .await
            .expect("the retried run exits 0");

        assert_eq!(*started.lock().unwrap(), 2);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], "Count the files in /data", "got {sent:?}");
    }

    /// A stall resume runs on what the stalled pass left of the run's budget.
    #[tokio::test]
    async fn a_stall_resume_gets_only_the_rest_of_the_runs_budget() {
        let tool = |i: usize| {
            [
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: format!("call_{i}"),
                    name: "shell_execute".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                    id: format!("call_{i}"),
                    result: "ok".into(),
                }),
            ]
        };
        let mut items: Vec<ScriptedItem> = (0..3).flat_map(tool).collect();
        items.push(ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
            StreamErrorKind::Stalled,
            stalled_stream_message(chatty_core::services::STALL_TIMEOUT),
        ))));
        let stalled = Scenario {
            name: "stall_after_three_tools",
            progress: Vec::new(),
            items,
        };
        let (mut runner, mut event_rx, _started, _workspace) =
            scripted_runner(vec![stalled, answer_turn("Done.")]).await;
        runner.execution_settings.max_agent_turns = 10;

        runner.send_message("summarize the repo".to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }
        runner.send_recovery_prompt(STALL_RESUME_PROMPT.to_string());
        while runner.is_streaming {
            let event = event_rx.recv().await.expect("turn events");
            runner.handle_event(event);
        }

        assert_eq!(
            runner.scripted_budgets,
            vec![
                Some(TurnBudget::run_share(10, 10, 0)),
                Some(TurnBudget::run_share(7, 10, 3)),
            ]
        );
    }

    /// Other errors keep their behaviour: one that is not retried ends the
    /// run on the spot, with no resume.
    #[tokio::test]
    async fn a_non_stall_error_is_not_resumed() {
        let other = Scenario {
            name: "other_error",
            progress: Vec::new(),
            items: vec![ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
                StreamErrorKind::Other,
                "max turns reached",
            )))],
        };
        let (runner, event_rx, started, _workspace) =
            scripted_runner(vec![other, answer_turn("never reached")]).await;

        let error = run_headless(runner, event_rx, "summarize the repo".to_string())
            .await
            .expect_err("an unrecovered error fails the run");

        assert!(error.to_string().contains("max turns reached"), "{error}");
        assert_eq!(*started.lock().unwrap(), 1);
    }
}
