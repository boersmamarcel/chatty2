//! Headless / pipe modes for `chatty-tui`.
//!
//! The TUI binary supports running without the Ratatui UI for scripting and
//! testing scenarios:
//!
//! - **`--headless`** — drive a single conversation programmatically with
//!   flags (model, prompt, tools), print the final assistant message to
//!   stdout, exit.
//! - **`--pipe`** — read a JSON-encoded request from stdin, run the
//!   conversation, write a JSON-encoded response to stdout.
//!
//! # What lives here
//!
//! - `run_headless`, `run_pipe` — the two entry functions called from `main`.
//! - `HeadlessRunner` (`runner.rs`) — the conversation, driven from an
//!   `AgentSession` directly with no terminal state (AGE-196).
//! - Helpers that wire the runner's events into stdout-only output (tool
//!   call summaries, token usage, errors).
//!
//! # What does NOT live here
//!
//! - The interactive Ratatui UI — `ui/`.
//! - The interactive engine — `engine/`.
//! - LLM streaming primitives — `chatty_core::services` and `factories`.

use anyhow::Result;
use chatty_core::services::agent_loop_guard::{PROGRESS_WINDOW_TOOL_CALLS, ProgressCheck};
use chatty_core::services::{
    AgentLoopGuard, HEADLESS_STALL_RESUME_ATTEMPTS, RecoveryAction, StreamError, StreamErrorKind,
    is_agent_todo_tool,
};
use tokio::sync::mpsc;

use crate::engine::ToolCallState;
use crate::events::AppEvent;

mod runner;
pub use runner::HeadlessRunner;

const MAX_TEXT_OVERFLOW_RECOVERY_ATTEMPTS: usize = 5;
const MAX_FINALIZATION_ATTEMPTS: usize = 4;
/// Tool results an answer-file run may spend exploring before headless stops
/// it for a finalization turn, as a share of the run's `max_agent_turns`
/// (see `answer_file_tool_budget`). 80 % sits just after chatty-core's
/// `TurnBudget` wrap-up note (75 %), so the model hears "start wrapping up"
/// once, gets a few turns to act on it, and only then is handed the
/// finalization turn — which, unlike TurnBudget's tool-free last word, can
/// still write the answer file.
const ANSWER_FILE_TOOL_BUDGET_PERCENT: usize = 80;
/// The budget never drops below this: a small cap (the default is 10) ends
/// the stream through TurnBudget first, and the post-stream finalization
/// catches a missing answer file there.
const MIN_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 16;
/// The budget when the run has no turn cap (`max_agent_turns == 0`).
const UNCAPPED_ANSWER_FILE_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 40;
/// Failed tool results (not counting sandbox/path-policy refusals) before a
/// finalization turn. Was 3, which one bad Python script plus a refusal
/// tripped while the model was still converging.
const MAX_FAILED_TOOL_RESULTS_BEFORE_FINALIZATION: usize = 8;
/// Tools that run a command whose output the model has not seen yet when an
/// answer file they wrote appears (`python3 count.py; echo -n 5 > answer.txt`).
const COMMAND_TOOLS: &[&str] = &["shell_execute", "execute_code"];
const FINALIZATION_ORIGINAL_PROMPT_CHARS: usize = 6_000;
const FINALIZATION_EVIDENCE_CHARS: usize = 16_000;
const FINALIZATION_TOOL_OUTPUT_CHARS: usize = 4_000;
const TEXT_HARD_STOP_BYTES: usize = 20_000;
const TEXT_OVERFLOW_RECOVERY_PROMPT: &str = "Stop reasoning — make ONE tool call now. If you already have the answer, call final_answer immediately. Do not write any analysis text before the tool call.";
/// Sent on the same history after the stall watchdog ended a turn: the
/// provider went quiet, not the task, so the model picks up where it was.
/// rig hands back a turn's tool round-trips only when the turn finishes,
/// so a stalled turn keeps its text but not its tool calls and results;
/// the prompt says so, or the model trusts results it can no longer see.
const STALL_RESUME_PROMPT: &str = "The previous response was interrupted by a stall. Its tool \
     calls and their results are not in the history, but files it wrote are still on disk. \
     Continue the task from where you left off: check the current state before redoing work.";
/// Sent on the same history after a provider error (a dropped connection,
/// an HTTP error status, a malformed tool call) ended a turn the model had
/// already said something in. The failed run's tool round-trips were
/// persisted with it (`failed_run_messages` in chatty-core), so the history
/// holds the work; the task is restated all the same. This used to be a
/// generic "continue the same benchmark task ... call final_answer with
/// output_path=/app/answer.txt" prompt, and on a SWE-bench run with no
/// answer file the model took it for a Q&A task and wrote /app/answer.txt
/// instead of fixing the code.
fn stream_error_recovery_prompt(original_prompt: &str, answer_file_required: bool) -> String {
    let mut prompt = String::from(
        "A provider error interrupted your last response. The tool calls and results above \
         are still valid and files you wrote are still on disk. Continue the same task from \
         where you left off: check the current state before redoing work.",
    );
    if answer_file_required {
        prompt.push_str(
            " When you have the answer, call final_answer with output_path=/app/answer.txt.",
        );
    }
    prompt.push_str(&task_reminder(original_prompt));
    prompt
}

/// Act on the progress check's verdict for a finished tool call; `true` when
/// it stopped the stream (the caller skips the rest of the event). A nudge
/// goes out the way a loop pivot does, once the cancelled turn has ended; a
/// finalization is the answer-file finalization pass when the run needs an
/// answer file, and a tool-free last pass otherwise.
fn on_no_progress(
    progress: ProgressCheck,
    answer_file_required: bool,
    engine: &mut HeadlessRunner,
    pending_loop_pivot_prompt: &mut Option<String>,
    finalization_pending_after_cancel: &mut bool,
    no_progress_final_pass_pending: &mut bool,
) -> bool {
    match progress {
        ProgressCheck::Fine => false,
        ProgressCheck::Nudge(nudge) => {
            eprintln!(
                "No progress in {PROGRESS_WINDOW_TOOL_CALLS} tool calls: nudging the model to change approach or finish."
            );
            pending_loop_pivot_prompt.get_or_insert(nudge);
            engine.stop_stream();
            true
        }
        ProgressCheck::Finalize => {
            eprintln!(
                "Still no progress {PROGRESS_WINDOW_TOOL_CALLS} tool calls after the nudge: stopping exploration for a final pass."
            );
            if answer_file_required {
                *finalization_pending_after_cancel = true;
            } else {
                *no_progress_final_pass_pending = true;
            }
            engine.stop_stream();
            true
        }
    }
}

/// The run's task, restated at the end of every continuation headless sends
/// after an error: a continuation that does not say what to continue left
/// the model guessing at the task from whatever the history still showed.
fn task_reminder(original_prompt: &str) -> String {
    format!(
        "\n\nThe task, unchanged:\n{}",
        original_task_excerpt(original_prompt)
    )
}

/// The run's last pass after its time budget ran out mid-turn: tools are
/// off, so the model answers from what it has.
const TIME_UP_PROMPT: &str = "Agent protocol follow-up: the time budget for this run is used \
     up and tools are now disabled. Reply now with your final answer, or with a short summary of \
     what you did and what remains.";

/// The last pass of a run the progress check stopped: two windows of
/// [`PROGRESS_WINDOW_TOOL_CALLS`] tool calls without a file written, a new
/// test run or a new file read. Tools are off, so the model answers from
/// what it has; the edits it made are on disk either way.
const NO_PROGRESS_FINAL_PROMPT: &str = "Agent protocol follow-up: your recent tool calls made \
     no progress (no file written, no new test run, no new file read), even after a nudge, so \
     tools are now disabled. Reply now with your best final result: what you changed, what state \
     the work is in, and what remains.";

/// Where a run with a time budget (`--max-duration`) stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeBudget {
    /// Before the deadline, or after it while the current pass ends by
    /// itself (the budget hook makes its next model call tool-free).
    Running,
    /// The deadline passed and the pass was stopped or failed before the
    /// model could answer; its last pass follows once the turn has ended.
    Cut,
    /// The last pass is out; whatever it ends with ends the run.
    LastPass,
}

/// How long past the deadline a pass may run before headless stops it: a
/// long tool call or model call can keep the budget hook from ever seeing
/// the next call. A tenth of the budget, between 5 s and 2 min.
fn deadline_grace(budget: std::time::Duration) -> std::time::Duration {
    (budget / 10).clamp(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(120),
    )
}

/// Recovery prompt for a hallucinated tool name (AGE-497): `error_message` is
/// rig's own `UnknownToolCall` display text, which already lists the
/// available and allowed tool names for this turn, so it is echoed verbatim
/// rather than paraphrased.
fn unknown_tool_call_recovery_prompt(error_message: &str) -> String {
    format!(
        "Your last tool call failed — {error_message} Call one of the listed available tools \
         with its exact name instead."
    )
}

/// Run in headless mode: send a message, collect the response, print to stdout.
pub async fn run_headless(
    mut engine: HeadlessRunner,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
    message: String,
) -> Result<()> {
    let answer_file_required = prompt_requires_answer_file(&[
        message.as_str(),
        engine.role_preamble().unwrap_or_default(),
    ]);

    // The run's clock starts with the run: `--max-duration` bounds the
    // work, not the process's startup.
    let deadline = engine.start_clock();
    let mut time_budget = TimeBudget::Running;
    // When headless itself stops a pass that overran the deadline.
    let mut backstop = deadline.map(|d| d.end() + deadline_grace(d.budget()));

    // Send message
    engine.send_message(message.clone());

    // Collect response
    let mut response = String::new();
    let mut text_overflow_attempts = 0usize;
    let mut finalization_attempts = 0usize;
    let mut tool_results_since_finalization = 0usize;
    let mut failed_tool_results_since_finalization = 0usize;
    let mut tool_budget_stop_requested = false;
    let mut failure_budget_stop_requested = false;
    let mut compact_file_finalization_sent = false;
    let mut last_compact_file_prompt: Option<String> = None;
    // `stop_stream()` only sets the cancel flag; `send_message()` right after
    // it is refused because `is_streaming` is still true (T3/AGE-242 — the
    // deferred-flag pattern used by `finalization_pending_after_cancel` and
    // `recovery_pending_after_error` below). These two hold the prompt until
    // `StreamCompleted` confirms the cancellation actually went through.
    let mut pending_compact_file_prompt: Option<String> = None;
    let mut pending_loop_pivot_prompt: Option<String> = None;
    let mut finalization_pending_after_cancel = false;
    // The session decides whether a stream error is retried and after how
    // long (AGE-273); the delay and the error that earned it are held here
    // until the turn has ended.
    let mut recovery_pending_after_error: Option<(std::time::Duration, StreamError)> = None;
    // A stream error the session would not retry ends the run as a failure
    // (AGE-401): the exit code says so, not an empty stdout.
    let mut unrecovered_error: Option<StreamError> = None;
    // The agent the turn is delegating to, for the trace's finish line.
    let mut delegated_agent: Option<String> = None;
    let mut infer_missing_answer = should_infer_missing_answer(&message);
    // Shared loop guard handles: repeated-tool-call detection, late-game deadline,
    // and per-turn verbosity tracking.
    let max_agent_turns = engine.execution_settings.max_agent_turns as usize;
    // Headless is unattended, so it also runs the busy-without-progress check.
    let mut loop_guard =
        AgentLoopGuard::new(max_agent_turns, answer_file_required).with_progress_check();
    // Set when the progress check asks for finalization on a run without an
    // answer file: the run's tool-free last pass follows once the turn ends,
    // and the run ends after it.
    let mut no_progress_final_pass_pending = false;
    let mut no_progress_final_pass_sent = false;
    let tool_budget = answer_file_tool_budget(max_agent_turns);
    // Set when a command tool wrote the answer file: the model gets one more
    // turn to see that command's output before the run stops.
    let mut answer_file_grace_turn = false;
    // Hard-stop flag set when loop_guard or the backstop threshold is exceeded.
    let mut text_overflow_stop_requested = false;
    let mut text_hard_stop_requested = false;
    let mut text_bytes_this_turn = 0usize;

    loop {
        let event = match backstop {
            Some(at) => tokio::select! {
                event = event_rx.recv() => event,
                _ = tokio::time::sleep_until(at.into()) => {
                    if time_budget == TimeBudget::Running && engine.is_streaming {
                        eprintln!(
                            "\nTime budget spent and the turn is still running; stopping it for a final answer."
                        );
                        time_budget = TimeBudget::Cut;
                        backstop = deadline.map(|d| {
                            std::time::Instant::now() + deadline_grace(d.budget())
                        });
                        engine.stop_stream();
                        continue;
                    }
                    if engine.is_streaming {
                        eprintln!("\nThe final pass overran the time budget; ending the run.");
                        engine.stop_stream();
                    }
                    break;
                }
            },
            None => event_rx.recv().await,
        };
        let Some(event) = event else {
            break;
        };
        match event {
            AppEvent::TextChunk(text) => {
                engine.handle_event(AppEvent::TextChunk(text.clone()));
                // Do not eprint assistant tokens: a parent follows the turn
                // through the runner's event observer, and the final answer
                // still goes to stdout.
                response.push_str(&text);
                text_bytes_this_turn += text.len();
                if answer_file_required
                    && !text_hard_stop_requested
                    && text_bytes_this_turn > TEXT_HARD_STOP_BYTES
                {
                    text_hard_stop_requested = true;
                    text_overflow_stop_requested = true;
                    eprintln!(
                        "\nText-only response exceeded hard limit; cancelling stream for compact finalization."
                    );
                    engine.stop_stream();
                }
                if !text_overflow_stop_requested {
                    // Soft stop via loop guard (4 KB).
                    // NOTE: We do NOT call engine.stop_stream() here — interrupting mid-stream
                    // causes JSON parse errors ("EOF while parsing a string at col N").
                    // Instead, we set the flag and inject a recovery prompt after the response
                    // completes naturally in the StreamCompleted handler.
                    if loop_guard.on_text_chunk(text.len()) {
                        text_overflow_stop_requested = true;
                        eprintln!(
                            "\nText-only response exceeded verbosity limit; will inject brevity prompt after response completes."
                        );
                    }
                }
            }
            AppEvent::ToolCallStarted { ref name, .. } => {
                text_bytes_this_turn = 0;
                let name_str = name.clone();
                engine.handle_event(event);
                if let Some(tc) = engine.transcript.tool_call_named(&name_str) {
                    eprintln!("\n{}", format_tool_call_header(tc));
                } else {
                    eprintln!("\n  \u{27f3} {}", name_str);
                }
            }
            AppEvent::ToolCallResult { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                let mut called_final_answer = false;
                let mut pivot_msg: Option<String> = None;
                let mut tool_failed = false;
                let mut wrote_by_command = false;
                let mut compact_file_extracted = false;
                let mut progress = ProgressCheck::Fine;
                if let Some(tc) = engine.transcript.tool_call(&id_str) {
                    eprintln!();
                    for line in format_tool_call_lines(tc) {
                        eprintln!("{line}");
                    }
                    // The plan card, rewritten after every todo result (AGE-136).
                    if is_agent_todo_tool(&tc.name)
                        && let Some(snapshot) = engine.transcript.plan.as_ref()
                    {
                        eprintln!();
                        for line in format_plan_lines(snapshot) {
                            eprintln!("{line}");
                        }
                    }
                    let status = if tool_result_looks_failed(tc) {
                        "err"
                    } else {
                        "ok"
                    };
                    if final_answer_ends_run(answer_file_required, &tc.name, &engine) {
                        if let Err(error) =
                            normalize_existing_answer_file_for_prompt(&engine, &message)
                        {
                            eprintln!("Answer file post-normalization skipped: {error}");
                        }
                        called_final_answer = true;
                    }
                    tool_failed = status == "err" && !tool_result_is_policy_refusal(tc);
                    wrote_by_command = COMMAND_TOOLS.contains(&tc.name.as_str());
                    compact_file_extracted =
                        compact_file_extraction_tool_result(answer_file_required, tc);
                    // Check for repeated identical tool call (loop detection).
                    pivot_msg = loop_guard.on_tool_completed(&tc.name, &tc.input);
                    progress = loop_guard.on_tool_progress(&tc.name, &tc.input);
                }
                if !called_final_answer
                    && answer_file_required
                    && engine.is_team_leader()
                    && answer_file_exists(&engine)
                {
                    // A worker's result, not the end of this turn (AGE-441);
                    // said out loud because a lone agent would stop here.
                    eprintln!(
                        "Answer file exists (a worker's or our own); the team leader keeps going."
                    );
                }
                if called_final_answer {
                    eprintln!("final_answer completed and answer file exists; stopping stream.");
                    engine.stop_stream();
                } else if stops_on_answer_file(&engine, answer_file_required) {
                    match answer_file_stop(answer_file_grace_turn, wrote_by_command) {
                        AnswerFileStop::Now => {
                            // Written by a dedicated write (write_file & co.),
                            // or the model has had its look at the command's
                            // output and acted again: stop so it doesn't
                            // loop rewriting the same answer.
                            eprintln!("Answer file exists after tool call; stopping stream early.");
                            engine.stop_stream();
                        }
                        AnswerFileStop::AfterNextTurn => {
                            // `python3 count.py; echo -n 5 > answer.txt`: the
                            // model has not seen what the command printed.
                            // One more model turn to read it (and rewrite the
                            // file if it disagrees); the next tool result, a
                            // final_answer, or the turn ending stops the run.
                            answer_file_grace_turn = true;
                            eprintln!(
                                "Answer file written by a command; letting the model see its output for one more turn."
                            );
                        }
                    }
                } else if !compact_file_finalization_sent
                    && compact_file_extracted
                    && !answer_file_exists(&engine)
                {
                    compact_file_finalization_sent = true;
                    infer_missing_answer = true;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    let compact_prompt = build_compact_file_answer_prompt(&engine, &message);
                    last_compact_file_prompt = Some(compact_prompt.clone());
                    eprintln!(
                        "Complete compact file extraction captured; requesting answer from evidence."
                    );
                    // Deferred: send once StreamCompleted confirms the
                    // cancellation went through (AGE-242 / D3).
                    pending_compact_file_prompt = Some(compact_prompt);
                    engine.stop_stream();
                    continue;
                } else if let Some(pivot) = pivot_msg {
                    eprintln!(
                        "Loop detected (pivot {}/{}): injecting strategy pivot prompt.",
                        loop_guard.loop_pivot_count(),
                        3
                    );
                    // Deferred: send once StreamCompleted confirms the
                    // cancellation went through (AGE-242 / D3).
                    pending_loop_pivot_prompt = Some(pivot);
                    engine.stop_stream();
                    tool_results_since_finalization = 0;
                    continue;
                } else if on_no_progress(
                    progress,
                    answer_file_required,
                    &mut engine,
                    &mut pending_loop_pivot_prompt,
                    &mut finalization_pending_after_cancel,
                    &mut no_progress_final_pass_pending,
                ) {
                    continue;
                }
                tool_results_since_finalization += 1;
                if tool_failed {
                    failed_tool_results_since_finalization += 1;
                }
                if should_stop_for_answer_file_tool_budget(
                    answer_file_required,
                    tool_results_since_finalization,
                    tool_budget,
                    finalization_attempts,
                    tool_budget_stop_requested,
                    &engine,
                ) {
                    tool_budget_stop_requested = true;
                    eprintln!(
                        "Answer file was not created after many tool calls; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                } else if should_stop_for_failed_tool_budget(
                    answer_file_required,
                    failed_tool_results_since_finalization,
                    finalization_attempts,
                    failure_budget_stop_requested,
                    &engine,
                ) {
                    failure_budget_stop_requested = true;
                    eprintln!(
                        "Several tool calls failed without creating an answer file; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                }
            }
            AppEvent::ToolCallError { ref id, .. } => {
                let id_str = id.clone();
                engine.handle_event(event);
                let mut refused = false;
                let mut progress = ProgressCheck::Fine;
                if let Some(tc) = engine.transcript.tool_call(&id_str) {
                    eprintln!();
                    for line in format_tool_call_lines(tc) {
                        eprintln!("{line}");
                    }
                    refused = tool_result_is_policy_refusal(tc);
                    progress = loop_guard.on_tool_progress(&tc.name, &tc.input);
                }
                if answer_file_grace_turn && stops_on_answer_file(&engine, answer_file_required) {
                    eprintln!("Answer file exists after the extra turn; stopping stream.");
                    engine.stop_stream();
                    continue;
                }
                if on_no_progress(
                    progress,
                    answer_file_required,
                    &mut engine,
                    &mut pending_loop_pivot_prompt,
                    &mut finalization_pending_after_cancel,
                    &mut no_progress_final_pass_pending,
                ) {
                    continue;
                }
                tool_results_since_finalization += 1;
                if !refused {
                    failed_tool_results_since_finalization += 1;
                }
                if should_stop_for_answer_file_tool_budget(
                    answer_file_required,
                    tool_results_since_finalization,
                    tool_budget,
                    finalization_attempts,
                    tool_budget_stop_requested,
                    &engine,
                ) {
                    tool_budget_stop_requested = true;
                    eprintln!(
                        "Answer file was not created after many tool calls; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                } else if should_stop_for_failed_tool_budget(
                    answer_file_required,
                    failed_tool_results_since_finalization,
                    finalization_attempts,
                    failure_budget_stop_requested,
                    &engine,
                ) {
                    failure_budget_stop_requested = true;
                    eprintln!(
                        "Several tool calls failed without creating an answer file; stopping exploration for a compact finalization pass."
                    );
                    engine.stop_stream();
                }
            }
            AppEvent::StreamCompleted => {
                engine.handle_event(AppEvent::StreamCompleted);
                // Update loop guard: resets per-turn counters and checks for late-game deadline.
                let turns_used = engine.transcript.assistant_turns();
                loop_guard.on_turn_complete(turns_used, answer_file_exists(&engine));
                let was_text_overflow = text_overflow_stop_requested;
                text_overflow_stop_requested = false;
                text_hard_stop_requested = false;
                text_bytes_this_turn = 0;
                // Out of time: no pivots, retries or nudges, only the run's
                // last pass, and only if it still needs one.
                if deadline.is_some_and(|d| d.is_past(std::time::Instant::now())) {
                    let cut = time_budget == TimeBudget::Cut
                        || recovery_pending_after_error.take().is_some()
                        || pending_loop_pivot_prompt.take().is_some()
                        || pending_compact_file_prompt.take().is_some()
                        || std::mem::take(&mut finalization_pending_after_cancel);
                    if time_budget == TimeBudget::LastPass {
                        break;
                    }
                    let needs_answer_file = answer_file_required && !answer_file_exists(&engine);
                    if !needs_answer_file && !cut {
                        // The pass ended by itself; its last model call was
                        // the budget hook's tool-free one, or the model was
                        // done anyway.
                        break;
                    }
                    time_budget = TimeBudget::LastPass;
                    backstop =
                        deadline.map(|d| std::time::Instant::now() + deadline_grace(d.budget()));
                    if needs_answer_file {
                        eprintln!(
                            "Time budget spent without an answer file; requesting a compact finalization pass."
                        );
                        send_answer_file_finalization_prompt(&mut engine, &message, cut);
                    } else {
                        eprintln!(
                            "Time budget spent mid-turn; asking for a final answer with tools disabled."
                        );
                        engine.send_time_up_pass(TIME_UP_PROMPT.to_string());
                    }
                    continue;
                }
                // The stop that led here was requested specifically to send
                // one of these; the cancellation has now gone through, so
                // send_message() will actually take (AGE-242 / D3).
                if let Some(compact_prompt) = pending_compact_file_prompt.take() {
                    send_compact_file_answer_prompt(&mut engine, compact_prompt);
                    continue;
                }
                if let Some(pivot) = pending_loop_pivot_prompt.take() {
                    engine.send_message(pivot);
                    continue;
                }
                if no_progress_final_pass_sent {
                    // The run's last pass after the progress check stopped it.
                    break;
                }
                if std::mem::take(&mut no_progress_final_pass_pending) {
                    no_progress_final_pass_sent = true;
                    engine.send_time_up_pass(NO_PROGRESS_FINAL_PROMPT.to_string());
                    continue;
                }
                if let Some((delay, error)) = recovery_pending_after_error.take() {
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    #[cfg(test)]
                    let delay = if engine.skip_recovery_delay {
                        std::time::Duration::ZERO
                    } else {
                        delay
                    };
                    if error.kind != StreamErrorKind::Stalled {
                        eprintln!(
                            "Retrying after stream error in {}s on the same history.",
                            delay.as_secs()
                        );
                    }
                    tokio::time::sleep(delay).await;
                    if let Some(message) = engine.take_rolled_back_message() {
                        // The turn failed before the model said anything, so
                        // it was rolled back with its prompt: send that
                        // same message again.
                        engine.send_recovery_prompt(message);
                    } else if error.kind == StreamErrorKind::Stalled {
                        engine.send_recovery_prompt(format!(
                            "{STALL_RESUME_PROMPT}{}",
                            task_reminder(&message)
                        ));
                    } else if error.kind == StreamErrorKind::UnknownToolCall {
                        // AGE-497: rig's own message already lists the
                        // available/allowed tool names, so it is worth
                        // re-sending verbatim instead of the generic prompt.
                        engine.send_recovery_prompt(format!(
                            "{}{}",
                            unknown_tool_call_recovery_prompt(&error.message),
                            task_reminder(&message)
                        ));
                    } else if let Some(compact_prompt) = last_compact_file_prompt.as_deref() {
                        // Carries the original task itself.
                        engine.send_recovery_prompt(build_compact_file_recovery_prompt(
                            compact_prompt,
                        ));
                    } else {
                        engine.send_recovery_prompt(stream_error_recovery_prompt(
                            &message,
                            answer_file_required,
                        ));
                    }
                    continue;
                }
                if was_text_overflow {
                    // Model generated too much text without calling a tool (response completed naturally).
                    // Inject a focused action prompt to redirect toward a tool call.
                    if text_overflow_attempts < MAX_TEXT_OVERFLOW_RECOVERY_ATTEMPTS {
                        text_overflow_attempts += 1;
                        tool_results_since_finalization = 0;
                        failed_tool_results_since_finalization = 0;
                        tool_budget_stop_requested = false;
                        failure_budget_stop_requested = false;
                        eprintln!(
                            "Text overflow (no tool call after 4KB): injecting action prompt ({}/{}).",
                            text_overflow_attempts, MAX_TEXT_OVERFLOW_RECOVERY_ATTEMPTS
                        );
                        engine.send_message(TEXT_OVERFLOW_RECOVERY_PROMPT.to_string());
                        continue;
                    }
                    // Exhausted recovery attempts — fall through to finalization.
                }
                // Late-game deadline: inject once when turns are almost exhausted.
                if let Some(deadline) = loop_guard.take_deadline_message() {
                    eprintln!(
                        "Late-game deadline prompt injected ({} turns used of {}).",
                        turns_used, max_agent_turns
                    );
                    engine.send_message(deadline);
                    continue;
                }
                if finalization_pending_after_cancel {
                    finalization_pending_after_cancel = false;
                    finalization_attempts += 1;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    let delay_secs = 15u64 * finalization_attempts as u64;
                    eprintln!(
                        "Answer file was not created after stopping exploration; requesting finalization in {}s.",
                        delay_secs
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                    send_answer_file_finalization_prompt(&mut engine, &message, true);
                    continue;
                }
                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_attempts += 1;
                    tool_results_since_finalization = 0;
                    failed_tool_results_since_finalization = 0;
                    tool_budget_stop_requested = false;
                    failure_budget_stop_requested = false;
                    eprintln!(
                        "Answer file was not created; requesting a compact finalization pass."
                    );
                    send_answer_file_finalization_prompt(&mut engine, &message, false);
                    continue;
                }
                break;
            }
            AppEvent::AgentProtocolFollowUp(prompt) => {
                engine.handle_event(AppEvent::AgentProtocolFollowUp(prompt.clone()));
                eprintln!("Agent protocol follow-up injected.");
                tool_results_since_finalization = 0;
                failed_tool_results_since_finalization = 0;
                tool_budget_stop_requested = false;
                failure_budget_stop_requested = false;
                continue;
            }
            AppEvent::Delegation(ref progress) => {
                // `invoke_agent` is rendered from its progress, not from a
                // tool row (`Transcript` suppresses those), so its input and
                // output are logged here to keep the trace auditable.
                for line in format_delegation_lines(progress, &mut delegated_agent) {
                    eprintln!("{line}");
                }
                engine.handle_event(event);
            }
            AppEvent::StreamError(error) => {
                engine.handle_event(AppEvent::StreamError(error.clone()));
                eprintln!("Error: {}", error);

                // Out of time: the run's last pass (or its end) follows once
                // the turn has ended, not a retry.
                if deadline.is_some_and(|d| d.is_past(std::time::Instant::now())) {
                    if time_budget == TimeBudget::Running {
                        time_budget = TimeBudget::Cut;
                    }
                    continue;
                }

                // A stale answer.txt says nothing about a run that asks for
                // none: it retries like any other.
                if answer_file_required && answer_file_exists(&engine) {
                    eprintln!(
                        "Answer file already exists; keeping the run for verifier evaluation."
                    );
                    break;
                }

                let retry_after = match engine.session.recovery_action(&error) {
                    RecoveryAction::Retry { after } => Some(after),
                    // A protocol nudge: re-prompt right away.
                    RecoveryAction::Nudge => Some(std::time::Duration::ZERO),
                    RecoveryAction::Stop => None,
                };
                if let Some(after) = retry_after {
                    if error.kind == StreamErrorKind::Stalled {
                        // Nobody is here to "send a message to continue", so
                        // this run sends it: the same history, one short
                        // prompt, a bounded number of times per task.
                        eprintln!(
                            "Auto-resuming the stalled turn ({}/{}).",
                            engine.session.recovery_attempts(error.kind),
                            HEADLESS_STALL_RESUME_ATTEMPTS
                        );
                    }
                    recovery_pending_after_error = Some((after, error));
                    continue;
                }

                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_pending_after_cancel = true;
                    continue;
                }

                unrecovered_error = Some(error);
                break;
            }
            AppEvent::StreamCancelled => {
                engine.handle_event(AppEvent::StreamCancelled);
                // Stopped for the time budget: `StreamCompleted` sends the
                // last pass.
                if time_budget == TimeBudget::Cut {
                    continue;
                }
                // A deferred prompt (loop-pivot or compact-file finalization)
                // is waiting for `StreamCompleted` to confirm the
                // cancellation went through (AGE-242 / D3). That arm fires
                // right after this one, so keep looping instead of breaking
                // here — otherwise the prompt is dropped and the run ends
                // with whatever was on disk (AGE-493).
                if pending_loop_pivot_prompt.is_some() || pending_compact_file_prompt.is_some() {
                    continue;
                }
                if should_request_answer_file_finalization(
                    answer_file_required,
                    finalization_attempts,
                    &engine,
                ) {
                    finalization_pending_after_cancel = true;
                    continue;
                }
                break;
            }
            AppEvent::ApprovalRequested {
                ref id,
                ref command,
                is_sandboxed,
            } => {
                let approval = crate::engine::ApprovalInfo {
                    id: id.clone(),
                    command: command.clone(),
                    is_sandboxed,
                    decision: None,
                };
                engine.handle_event(event);
                eprintln!("\n{}", format_approval_requested(&approval));
            }
            AppEvent::ApprovalResolved { approved, .. } => {
                engine.handle_event(event);
                eprintln!("{}", format_approval_resolved(approved));
            }
            // Handle other events silently
            _ => {
                engine.handle_event(event);
            }
        }
    }

    if answer_file_required
        && infer_missing_answer
        && !answer_file_exists(&engine)
        && let Some(candidate) = infer_answer_candidate(&engine, &response, &message)
    {
        match write_inferred_answer_file(&engine, &candidate) {
            Ok(path) => eprintln!(
                "Answer file was missing; wrote inferred compact answer candidate '{}' to {}.",
                candidate,
                path.display()
            ),
            Err(error) => eprintln!(
                "Answer file was missing and inferred candidate '{}' could not be written: {}",
                candidate, error
            ),
        }
    }

    if let Some(error) = unrecovered_error {
        // Whatever was streamed before the failure is still worth having;
        // the exit code is what tells a script (or a leader) it is not an
        // answer.
        if !response.trim().is_empty() {
            println!("{}", response);
        }
        anyhow::bail!("the turn ended with an unrecovered stream error: {error}");
    }

    // Print response to stdout
    println!("{}", response);

    Ok(())
}

/// Whether a `tool_name` result ends the run as its final answer: only in a
/// run whose task asks for an answer file, and once that file exists. In a
/// coding run with no answer file, a final_answer carrying a diagnosis
/// (and an answer.txt left in the workspace) stopped the run before the fix
/// was made.
fn final_answer_ends_run(
    answer_file_required: bool,
    tool_name: &str,
    engine: &HeadlessRunner,
) -> bool {
    answer_file_required && tool_name == "final_answer" && answer_file_exists(engine)
}

/// Whether a tool result ends the turn because the task's answer file now
/// exists (AGE-441).
///
/// A lone `--headless` agent — and a worker, which never runs with `--team`
/// — is done once the file is there: staying in the loop only rewrites the
/// same answer. A `--team` leader shares the workspace with its workers, so
/// the file is one of *their* results; its own turn ends when its delegation
/// flow does, or its turns run out, never on a file somebody else wrote.
fn stops_on_answer_file(engine: &HeadlessRunner, answer_file_required: bool) -> bool {
    answer_file_required && !engine.is_team_leader() && answer_file_exists(engine)
}

/// What an answer file that just appeared after a tool result means for
/// the stream.
#[derive(Debug, PartialEq, Eq)]
enum AnswerFileStop {
    /// Stop the stream now.
    Now,
    /// Let the model see this tool result for one more turn first.
    AfterNextTurn,
}

/// A dedicated write of the answer (write_file, apply_diff, ...) holds no
/// output the model has not seen, so it stops the run at once. A command
/// (`shell_execute`, `execute_code`) that computes and writes in one go does
/// — `python3 count.py; echo -n 5 > answer.txt` wrote 5 while the script
/// printed 7 — so the model gets one more turn to read it. Once that grace
/// turn has been given (`grace_given`), any further tool result stops.
fn answer_file_stop(grace_given: bool, wrote_by_command: bool) -> AnswerFileStop {
    if wrote_by_command && !grace_given {
        AnswerFileStop::AfterNextTurn
    } else {
        AnswerFileStop::Now
    }
}

fn should_infer_missing_answer(original_prompt: &str) -> bool {
    match std::env::var("CHATTY_INFER_MISSING_ANSWER")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("0" | "false" | "no") => false,
        Some("1" | "true" | "yes") => true,
        _ => prompt_has_strict_answer_format(original_prompt),
    }
}

fn prompt_has_strict_answer_format(original_prompt: &str) -> bool {
    let prompt = original_prompt.to_ascii_lowercase();
    prompt.contains("answer must be")
        && prompt.contains("format")
        && (prompt.contains(":{") || prompt.contains("}:") || prompt.contains(":"))
}

/// Run in pipe mode: read stdin, send as message, print response to stdout.
pub async fn run_pipe(
    engine: HeadlessRunner,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        eprintln!("No input provided on stdin");
        std::process::exit(1);
    }

    run_headless(engine, event_rx, input).await
}

mod answer_file;
mod recovery;
mod tool_format;

use answer_file::*;
use recovery::*;
use tool_format::*;

#[cfg(test)]
mod tests;
