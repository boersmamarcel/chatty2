//! AGE-941: a leader whose workers' results the guard shapes away gets the
//! results ledger, and nothing else changes.

use super::*;
use crate::services::results_ledger::{LEDGER_HEADER, figures_not_in};
use futures::StreamExt;
use rig_agent::agent::{AgentBuilder, MultiTurnStreamItem};
use rig_agent::test_utils::MockToolError;
use rig_agent::tool::{Tool, ToolContext};
use rig_core::completion::message::AssistantContent;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

const TASK: &str = "Coordinate a 10-chapter guide: for each chapter delegate a draft \
                    to data-analyst and a review to reviewer, then reply with a table.";
const CHAPTERS: usize = 10;

fn counter() -> TokenCounter {
    TokenCounter::for_model("test")
}

fn settings() -> ContextShaperSettings {
    ContextShaperSettings {
        response_reserve: 0,
        headroom_fraction: 0.0,
        ..ContextShaperSettings::default()
    }
}

/// Chapter `i`'s worked-example result: distinct for every chapter.
fn figure(i: usize) -> String {
    format!("{}.{:02}", 300 + i * 11, (i * 7) % 100)
}

fn prose(words: usize) -> String {
    vec!["prose"; words].join(" ")
}

fn draft(i: usize) -> String {
    format!(
        "# Chapter {i}\n\n{}\nWorked example: total = {} over the sample\n{}",
        prose(600),
        figure(i),
        prose(100)
    )
}

/// What the data-analyst returns for `task` ("Draft chapter i") and the
/// reviewer for "Review chapter i", whose task carries the draft, as the
/// soak's leader sent it.
fn worker_answer(task: &str) -> (String, String) {
    let first = task.lines().next().unwrap_or_default();
    let i: usize = first
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a chapter number");
    if first.starts_with("Draft") {
        ("data-analyst".to_string(), draft(i))
    } else {
        (
            "reviewer".to_string(),
            format!(
                "APPROVE\n1. Recomputed total = {} ✓\n{}",
                figure(i),
                prose(30)
            ),
        )
    }
}

fn tasks() -> Vec<String> {
    (1..=CHAPTERS)
        .flat_map(|i| {
            [
                format!("Draft chapter {i}"),
                format!("Review chapter {i}\n\n{}", draft(i)),
            ]
        })
        .collect()
}

fn call(id: &str, task: &str) -> Message {
    let agent = if task.starts_with("Draft") {
        "data-analyst"
    } else {
        "reviewer"
    };
    Message::Assistant(rig_core::message::AssistantMessage::new(vec![
        AssistantContent::tool_call(
            id,
            crate::testing::tool_name("invoke_agent"),
            serde_json::json!({ "agent": agent, "prompt": task }),
        ),
    ]))
}

fn result(id: &str, name: &str, value: serde_json::Value) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            call: CallId::from_wire(id),
            name: crate::testing::tool_name(name),
            content: vec![ToolResultContent::Json { value }],
            is_error: false,
        })],
    }
}

/// The soak's shape: the task, then a draft and a review per chapter.
fn leader_run() -> Vec<Message> {
    let mut history = vec![Message::user(TASK)];
    for (n, task) in tasks().iter().enumerate() {
        let id = format!("d{n}");
        let (agent, response) = worker_answer(task);
        history.push(call(&id, task));
        history.push(result(
            &id,
            "invoke_agent",
            serde_json::json!({ "agent": agent, "response": response, "success": true }),
        ));
    }
    history
}

fn all_text(messages: &[Message]) -> String {
    serde_json::to_string(messages).unwrap()
}

fn ledger_index(messages: &[Message]) -> Option<usize> {
    messages.iter().position(|m| match m {
        Message::User { content } => content
            .iter()
            .any(|c| matches!(c, UserContent::Text(t) if t.text.starts_with(LEDGER_HEADER))),
        _ => false,
    })
}

fn tokens(messages: &[Message]) -> usize {
    let counter = counter();
    messages.iter().map(|m| counter.count_message(m)).sum()
}

#[tokio::test]
async fn a_leader_history_that_fits_goes_out_as_recorded() {
    let history = leader_run();
    let prompt = Message::user("continue");
    let shaper = ContextShaper::new(settings(), counter(), Some(tokens(&history) * 2));
    assert!(shaper.prepare_request(&history, &prompt, 1).await.is_none());
}

/// An ordinary single-agent run the guard shapes is shaped exactly as
/// before: no delegation, no ledger.
#[tokio::test]
async fn a_shaped_run_without_delegations_is_unchanged() {
    let mut history = vec![Message::user(TASK)];
    for n in 0..20 {
        let id = format!("r{n}");
        history.push(Message::Assistant(
            rig_core::message::AssistantMessage::new(vec![AssistantContent::tool_call(
                &id,
                crate::testing::tool_name("read_file"),
                serde_json::json!({ "path": format!("f{n}.rs") }),
            )]),
        ));
        history.push(result(
            &id,
            "read_file",
            serde_json::json!({ "content": prose(200) }),
        ));
    }
    let prompt = Message::user("continue");
    let shaper = ContextShaper::new(settings(), counter(), Some(tokens(&history) / 3));
    let expected = shaper
        .request_history(&history, &prompt, 1)
        .expect("shaped");
    let sent = shaper
        .prepare_request(&history, &prompt, 1)
        .await
        .expect("shaped");
    assert_eq!(sent, expected);
    assert!(ledger_index(&sent).is_none());
}

/// The soak's final call, replayed: the guard snips the middle of a
/// 20-delegation history, and the request still carries every chapter's
/// figure — in the ledger right after the task, or verbatim in the tail.
#[tokio::test]
async fn a_snipped_leader_request_carries_every_workers_figures() {
    let history = leader_run();
    let prompt = Message::user("All chapters are done. Reply with the table.");
    // A third of what the run needs, like the soak's 32k window under
    // ~75k tokens of history.
    let window = tokens(&history) / 3;
    let shaper = ContextShaper::new(settings(), counter(), Some(window));
    let plain = shaper
        .request_history(&history, &prompt, 1)
        .expect("shaped");
    let plain_text = all_text(&plain);
    assert!(plain_text.contains("messages snipped"), "the guard snips");
    let lost: Vec<usize> = (1..=CHAPTERS)
        .filter(|i| !plain_text.contains(&figure(*i)))
        .collect();
    assert!(
        lost.len() >= 5,
        "without the ledger most figures are gone: {lost:?}"
    );

    let sent = shaper
        .prepare_request(&history, &prompt, 1)
        .await
        .expect("shaped");
    let text = all_text(&sent);
    for i in 1..=CHAPTERS {
        assert!(
            text.contains(&figure(i)),
            "chapter {i}'s figure is in the request"
        );
    }
    let ledger = all_text(&sent[ledger_index(&sent).expect("a ledger")..][..1]);
    for i in &lost {
        assert!(
            ledger.contains(&figure(*i)),
            "chapter {i}'s figure is in the ledger"
        );
    }
    assert!(all_text(&sent[1..2]).contains("messages snipped"));
    assert_eq!(
        ledger_index(&sent),
        Some(2),
        "after the head (the task and the snip marker), ahead of every kept round trip"
    );
    assert!(tool_round_trips_intact(&sent, &[]));
    let budget = shaper.history_budget(counter().count_message(&prompt));
    assert!(tokens(&sent) <= budget, "{} > {budget}", tokens(&sent));
}

/// A stand-in for `invoke_agent`: answers each task with the scripted
/// worker answer, in the real tool's JSON shape.
struct FakeInvokeAgent;

#[derive(serde::Deserialize)]
struct FakeArgs {
    prompt: String,
}

impl Tool for FakeInvokeAgent {
    const NAME: &'static str = "invoke_agent";
    type Error = MockToolError;
    type Args = FakeArgs;
    type Output = serde_json::Value;

    fn description(&self) -> String {
        "Delegate a task to an agent".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "agent": { "type": "string" }, "prompt": { "type": "string" } },
            "required": ["agent", "prompt"],
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let (agent, response) = worker_answer(&args.prompt);
        Ok(serde_json::json!({ "agent": agent, "response": response, "success": true }))
    }
}

/// On rig's own loop: a scripted leader delegates every draft and review,
/// then answers. The request its final answer is written from carries the
/// ledger and every chapter's figure; a final answer built from it has no
/// figure absent from the workers' results, and the check flags one that
/// does (the soak's "313.1% of sales").
#[tokio::test]
async fn a_scripted_leaders_final_request_is_grounded_in_its_workers_results() {
    let mut turns: Vec<Vec<MockStreamEvent>> = tasks()
        .iter()
        .enumerate()
        .map(|(n, task)| {
            let agent = if task.starts_with("Draft") {
                "data-analyst"
            } else {
                "reviewer"
            };
            vec![
                MockStreamEvent::tool_call(
                    format!("tool_call_{n}"),
                    "invoke_agent",
                    serde_json::json!({ "agent": agent, "prompt": task }),
                )
                .with_call_id(format!("call_{n}")),
                MockStreamEvent::final_response_with_total_tokens(4),
            ]
        })
        .collect();
    let table: String = (1..=CHAPTERS)
        .map(|i| format!("| {i} | 1 | APPROVE | total = {} |\n", figure(i)))
        .collect();
    turns.push(vec![
        MockStreamEvent::text(table.clone()),
        MockStreamEvent::final_response_with_total_tokens(4),
    ]);
    let model = MockCompletionModel::from_stream_turns(turns);
    let window = tokens(&leader_run()) / 3;
    let shaper = ContextShaper::new(settings(), counter(), Some(window));
    let agent = AgentBuilder::new(model.clone())
        .tool(FakeInvokeAgent)
        .add_hook(shaper.clone())
        .build();
    let mut stream = agent.prompt(TASK).max_turns(2 * CHAPTERS + 2).stream();
    let mut answer = None;
    while let Some(item) = stream.next().await {
        if let MultiTurnStreamItem::FinalResponse(response) = item.expect("the run succeeds") {
            answer = Some(response.output());
        }
    }
    assert_eq!(answer.as_deref(), Some(table.as_str()));

    let requests = model.requests();
    assert_eq!(requests.len(), 2 * CHAPTERS + 1);
    let last: Vec<Message> = requests
        .last()
        .unwrap()
        .chat_history
        .iter()
        .map(|m| (**m).clone())
        .collect();
    let text = all_text(&last);
    assert!(
        text.contains("messages snipped"),
        "the final call was shaped"
    );
    assert!(
        ledger_index(&last).is_some(),
        "the final call carries the ledger"
    );
    for i in 1..=CHAPTERS {
        assert!(
            text.contains(&figure(i)),
            "chapter {i}'s figure reached the final call"
        );
    }

    let results: Vec<String> = tasks().iter().map(|t| worker_answer(t).1).collect();
    let sources: Vec<&str> = results.iter().map(String::as_str).collect();
    assert!(
        figures_not_in(&table, &sources).is_empty(),
        "nothing invented"
    );
    let invented = format!("{table}| 1 | top 3 customers = 313.1% of sales |");
    assert_eq!(figures_not_in(&invented, &sources), ["313.1"]);
}
