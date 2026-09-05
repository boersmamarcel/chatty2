//! Scripted stream fixtures for characterization tests (AGE-191).
//!
//! Phases 1–4 of [AGE-190] move the turn-orchestration code that produces every
//! visible agent behaviour. These fixtures are the contract those phases must
//! not change: one scripted [`ResponseStream`] per scenario, driven through
//! whichever loop a frontend uses, with the resulting event sequence recorded as
//! a golden file.
//!
//! Both frontends script the *same* scenarios from here, so a divergence
//! between the two loops shows up as a difference between two golden files
//! rather than as a behaviour nobody notices.
//!
//! # What is contract and what is incidental
//!
//! Pinned by these fixtures:
//!
//! * The **order** of events within one scenario, and which chunk produces which
//!   event.
//! * Which chunks **terminate** the loop (`Done`, `Error`, a transport `Err`)
//!   and which merely pass through.
//! * That cancellation produces a cancelled outcome rather than a completed one.
//! * That sub-agent progress is drained ahead of stream chunks — the
//!   `biased` ordering in [`run_stream_loop`](super::run_stream_loop).
//!
//! Deliberately *not* pinned:
//!
//! * The real-time **interleaving** of progress events with stream chunks. In
//!   production a tool sends progress while the provider is mid-response, so the
//!   interleave is genuinely racy. Scenarios queue progress before the chunks
//!   they accompany, which makes the golden deterministic without claiming the
//!   racy order is a contract.
//! * Wall-clock timing, and anything derived from it (the stall watchdog has its
//!   own tests in [`super::stream_processor`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::models::clarification_store::ClarifyingQuestion;
use crate::models::message_types::ToolSource;
use crate::models::token_usage::ApiCallUsage;
use crate::services::llm_service::{ResponseStream, StreamChunk};
use crate::tools::invoke_agent_tool::InvokeAgentProgress;

/// One step in a scripted stream.
pub enum ScriptedItem {
    /// The stream yields this chunk.
    Chunk(StreamChunk),
    /// The stream yields `Err` — a transport or provider failure, which is a
    /// different path from [`StreamChunk::Error`] in both frontends.
    Failure(String),
    /// Sets the cancellation flag, then yields this chunk.
    ///
    /// Models the common Stop: the flag flips while a chunk is already in
    /// flight. The loop handles that chunk, then sees the flag on its next pass
    /// — the loop-top check, which is the primary cancellation path. Items after
    /// this one are never reached.
    ///
    /// The other path, where the flag flips while the provider is quiet and only
    /// the idle tick notices, is timing-dependent and is covered by
    /// [`super::stream_processor`]'s own tests rather than by a scenario here.
    CancelThen(StreamChunk),
}

/// A named scenario: sub-agent progress to queue up front, then a chunk script.
pub struct Scenario {
    /// Golden-file stem, e.g. `tool_call_then_result`.
    pub name: &'static str,
    /// Queued into the progress channel before the loop starts. See the module
    /// docs on why these are not interleaved with `items`.
    pub progress: Vec<InvokeAgentProgress>,
    pub items: Vec<ScriptedItem>,
}

/// Build a [`ResponseStream`] from a script.
///
/// `cancel_flag` must be the same flag the loop under test checks, so
/// [`ScriptedItem::CancelAndHang`] can reach it.
pub fn scripted_stream(items: Vec<ScriptedItem>, cancel_flag: Arc<AtomicBool>) -> ResponseStream {
    Box::pin(async_stream::stream! {
        for item in items {
            match item {
                ScriptedItem::Chunk(chunk) => yield Ok(chunk),
                ScriptedItem::Failure(message) => yield Err(anyhow::anyhow!(message)),
                ScriptedItem::CancelThen(chunk) => {
                    cancel_flag.store(true, Ordering::Relaxed);
                    yield Ok(chunk);
                }
            }
        }
    })
}

fn question(id: &str, text: &str) -> ClarifyingQuestion {
    ClarifyingQuestion {
        id: id.to_string(),
        question: text.to_string(),
        options: vec!["Yes".to_string(), "No".to_string()],
    }
}

/// The nine scenarios both frontends are characterized against.
///
/// Values are fixed strings, not generated: a golden file is only useful if
/// re-running the test produces byte-identical output.
pub fn scenarios() -> Vec<Scenario> {
    vec![
        // 1. A plain answer with no tools. The floor: text in, text out, Done.
        Scenario {
            name: "text_only",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Hello".into())),
                ScriptedItem::Chunk(StreamChunk::Text(", world".into())),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 2. The ordinary tool round-trip: started → input → result → answer.
        Scenario {
            name: "tool_call_then_result",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call-1".into(),
                    name: "read_file".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallInput {
                    id: "call-1".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                    id: "call-1".into(),
                    result: "# Chatty".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Text("It is the readme.".into())),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 3. A failing tool. The `Error:` prefix is what marks it as a failure
        //    downstream — see `llm_service::tool_result_looks_like_error`.
        Scenario {
            name: "tool_error",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call-1".into(),
                    name: "run_shell".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallInput {
                    id: "call-1".into(),
                    arguments: r#"{"command":"false"}"#.into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallError {
                    id: "call-1".into(),
                    error: "Error: run_shell: exited with status 1".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Text("That command failed.".into())),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 4. An approval the user grants: the tool then runs and reports back.
        Scenario {
            name: "approval_granted",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call-1".into(),
                    name: "run_shell".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ApprovalRequested {
                    id: "approval-1".into(),
                    command: "rm -rf build".into(),
                    is_sandboxed: false,
                }),
                ScriptedItem::Chunk(StreamChunk::ApprovalResolved {
                    id: "approval-1".into(),
                    approved: true,
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                    id: "call-1".into(),
                    result: "removed 'build'".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 5. The same approval denied: the tool result is a refusal, not a
        //    transport error, and the turn still completes normally.
        Scenario {
            name: "approval_denied",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call-1".into(),
                    name: "run_shell".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ApprovalRequested {
                    id: "approval-1".into(),
                    command: "rm -rf build".into(),
                    is_sandboxed: false,
                }),
                ScriptedItem::Chunk(StreamChunk::ApprovalResolved {
                    id: "approval-1".into(),
                    approved: false,
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallError {
                    id: "call-1".into(),
                    error: "Error: run_shell: the user denied this command".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 6. Stop pressed mid-answer. No Done, no Error — a cancelled outcome.
        Scenario {
            name: "cancelled_mid_stream",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Working on ".into())),
                ScriptedItem::CancelThen(StreamChunk::Text("it".into())),
                // Never reached; present so a regression that keeps reading the
                // stream past cancellation shows up in the golden.
                ScriptedItem::Chunk(StreamChunk::Text(" some more".into())),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 7. A sub-agent reporting progress through the invoke_agent channel
        //    rather than through the stream.
        Scenario {
            name: "sub_agent_progress",
            progress: vec![
                InvokeAgentProgress::Started {
                    agent_name: "researcher".into(),
                    prompt: "Summarize the changelog".into(),
                    source: ToolSource::Local,
                },
                InvokeAgentProgress::Text("Reading CHANGELOG.md".into()),
                InvokeAgentProgress::Finished {
                    success: true,
                    result: Some("Three releases since 0.3.45.".into()),
                },
            ],
            items: vec![
                ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                    id: "call-1".into(),
                    name: "invoke_agent".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                    id: "call-1".into(),
                    result: "Three releases since 0.3.45.".into(),
                }),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 8. Usage arriving before Done, which is where the cost figures and the
        //    trace attached to the finished turn come from. One per-call record
        //    precedes the aggregate, as rig emits them.
        Scenario {
            name: "token_usage_on_done",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Answer.".into())),
                ScriptedItem::Chunk(StreamChunk::ApiCallUsage(ApiCallUsage {
                    turn: 1,
                    input_tokens: 234,
                    cache_read_tokens: 1000,
                    cache_write_tokens: 0,
                    output_tokens: 56,
                })),
                ScriptedItem::Chunk(StreamChunk::TokenUsage {
                    input_tokens: 234,
                    output_tokens: 56,
                    cache_read_tokens: 1000,
                    cache_write_tokens: 0,
                }),
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
        // 9. The provider dying mid-turn: `Err` on the stream, which is a
        //    different path from a `StreamChunk::Error` the provider reported.
        Scenario {
            name: "provider_error_mid_stream",
            progress: Vec::new(),
            items: vec![
                ScriptedItem::Chunk(StreamChunk::Text("Partial ".into())),
                ScriptedItem::Failure("connection reset by peer".into()),
                // Never reached: the loop stops on the failure above.
                ScriptedItem::Chunk(StreamChunk::Done),
            ],
        },
    ]
}

/// A clarification scenario, kept out of [`scenarios`] because only the desktop
/// renders it today. Both frontends still map the chunk, so both record it.
pub fn clarification_scenario() -> Scenario {
    Scenario {
        name: "clarification_requested",
        progress: Vec::new(),
        items: vec![
            ScriptedItem::Chunk(StreamChunk::ClarificationRequested {
                id: "clarify-1".into(),
                questions: vec![question("q1", "Deploy to production?")],
            }),
            ScriptedItem::Chunk(StreamChunk::Done),
        ],
    }
}

// ---------------------------------------------------------------------------
// Golden files
// ---------------------------------------------------------------------------

/// Compare a recorded event sequence against a committed golden file.
///
/// `dir` is the calling crate's golden directory — each frontend keeps its own,
/// because they record different event types. Set `UPDATE_GOLDENS=1` to rewrite
/// them after a *deliberate* behaviour change; the diff in review is then the
/// record of what changed.
pub fn assert_golden(dir: &std::path::Path, scenario_name: &str, events: &[String]) {
    let path = dir.join(format!("{scenario_name}.txt"));
    let recorded = format!("{}\n", events.join("\n"));

    if std::env::var("UPDATE_GOLDENS").is_ok() {
        std::fs::create_dir_all(dir).expect("failed to create golden directory");
        std::fs::write(&path, &recorded).expect("failed to write golden");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}. Re-run with UPDATE_GOLDENS=1 to create it.",
            path.display()
        )
    });

    assert_eq!(
        expected, recorded,
        "\nevent sequence for `{scenario_name}` changed.\n\
         This is the contract AGE-190's phases must preserve. If the change is \
         deliberate, re-run with UPDATE_GOLDENS=1 and explain the diff in review.\n\
         --- golden ---\n{expected}\n--- recorded ---\n{recorded}"
    );
}
