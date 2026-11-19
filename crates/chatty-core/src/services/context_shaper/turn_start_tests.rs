//! AGE-994: a new turn on a long conversation. At turn start the history is
//! every earlier turn, most of it assistant text no stage before the trim
//! shrinks; the turn's first request must still fit the window, with the
//! task's first message, the latest user message and every tool round-trip
//! intact — or the run stops with an error saying why, before any request.

use super::*;
use futures::StreamExt;
use rig_agent::agent::AgentBuilder;
use rig_agent::test_utils::MockAddTool;
use rig_core::completion::message::AssistantContent;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

const TASK: &str = "Answer the DABstep question: which merchant paid the most fees in 2023?";
const WINDOW: usize = 32_768;
/// The preamble plus the full native tool set's schemas on a 32k model.
const BASE: usize = 16_000;

fn counter() -> TokenCounter {
    TokenCounter::for_model("test")
}

fn tokens(messages: &[Message]) -> usize {
    let counter = counter();
    messages.iter().map(|m| counter.count_message(m)).sum()
}

fn words(n: usize, tag: &str) -> String {
    format!("{tag}: {}", vec!["analysis"; n].join(" "))
}

/// One turn: the user's message, four round-trips whose calls carry a
/// plan and code, and a long final answer.
fn turn(user: &str, n: usize) -> Vec<Message> {
    let mut messages = vec![Message::user(user)];
    for r in 0..4 {
        let id = format!("t{n}-{r}");
        messages.push(Message::Assistant(
            rig_core::message::AssistantMessage::new(vec![
                AssistantContent::text(words(300, &format!("plan {id}"))),
                AssistantContent::tool_call(
                    &id,
                    crate::testing::tool_name("execute_code"),
                    serde_json::json!({ "code": words(1_500, "code") }),
                ),
            ]),
        ));
        messages.push(Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: CallId::from_wire(&id),
                name: crate::testing::tool_name("execute_code"),
                content: vec![ToolResultContent::Text(Text::new("fee 12.34 EUR"))],
                is_error: false,
            })],
        });
    }
    messages.push(Message::assistant(words(3_000, &format!("answer {n}"))));
    messages
}

/// The task's turn, then "continue" turns, to at least `target` tokens.
fn conversation(target: usize) -> Vec<Message> {
    let mut history = turn(TASK, 0);
    let mut n = 1;
    while tokens(&history) < target {
        history.extend(turn(&format!("continue, part {n}"), n));
        n += 1;
    }
    history
}

fn shaper(window: Option<usize>) -> ContextShaper {
    let shaper = ContextShaper::new(ContextShaperSettings::default(), counter(), window);
    shaper.set_base_tokens(BASE);
    shaper
}

fn text_of(message: &Message) -> Option<String> {
    match message {
        Message::User { content } => content.iter().find_map(|item| match item {
            UserContent::Text(t) => Some(t.text.clone()),
            _ => None,
        }),
        Message::Assistant(rig_core::message::AssistantMessage { content, .. }) => {
            content.iter().find_map(|item| match item {
                AssistantContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
        }
        Message::System { content } => Some(content.clone()),
    }
}

#[tokio::test]
async fn a_new_turn_on_a_long_conversation_is_shaped_to_fit() {
    let history = conversation(40_000);
    let prompt = Message::user("continue the task");
    let shaper = shaper(Some(WINDOW));
    let budget = shaper.history_budget(counter().count_message(&prompt));

    let sent = shaper
        .prepare_request(&history, &prompt, 0)
        .await
        .expect("a 40k history is shaped");

    assert!(tokens(&sent) <= budget, "{} > {budget}", tokens(&sent));
    assert_eq!(sent[0], history[0], "the task's first message is whole");
    assert_eq!(sent.last(), history.last(), "the last message is whole");
    let latest = latest_user_message(&history).unwrap();
    assert!(
        sent.contains(&history[latest]),
        "the latest user message survives"
    );
    assert!(tool_round_trips_intact(&sent, &[]));
    assert_eq!(shaper.window_overflow(&sent, &prompt), None);
}

/// Mid-run on the new turn: its user message is older than the kept
/// tail, and still sent.
#[tokio::test]
async fn the_latest_user_message_survives_the_snip() {
    let mut history = conversation(40_000);
    history.extend(turn("now check the 2022 fees too", 99));
    history.pop();
    let prompt = Message::user("go on");
    let shaper = shaper(Some(WINDOW));

    let sent = shaper
        .prepare_request(&history, &prompt, 3)
        .await
        .expect("shaped");

    assert!(
        sent.iter()
            .any(|m| text_of(m).as_deref() == Some("now check the 2022 fees too"))
    );
    assert_eq!(sent[0], history[0]);
    assert!(tool_round_trips_intact(&sent, &[]));
}

#[tokio::test]
async fn a_short_conversation_is_sent_as_recorded() {
    let history = turn(TASK, 0);
    let shaper = shaper(Some(200_000));
    assert_eq!(
        shaper
            .prepare_request(&history, &Message::user("continue"), 0)
            .await,
        None
    );
}

/// The trim drops a call with its results, never one without the other.
#[test]
fn the_trim_drops_whole_round_trips() {
    let history = conversation(40_000);
    let shaper = shaper(Some(WINDOW));
    let shaped = shaper.shape(&history, 10).expect("over budget");
    assert_eq!(shaped.stage_applied, ContextShaperStage::Trim);
    assert!(tool_round_trips_intact(&shaped.messages, &[]));
    assert!(shaped.tokens_after <= shaper.history_budget(10));
    let marker = shaped
        .messages
        .iter()
        .filter_map(text_of)
        .find(|t| t.starts_with("[CONTEXT SHAPER:"))
        .expect("one marker");
    let dropped: usize = marker
        .trim_start_matches("[CONTEXT SHAPER: ")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    // Everything but the marker is a message of the history.
    assert_eq!(dropped, history.len() - (shaped.messages.len() - 1));
}

/// A first message larger than the window: nothing shaping may take
/// makes it fit, so the request is not sent.
#[test]
fn a_history_that_cannot_fit_is_an_error_not_a_request() {
    let history = vec![
        Message::user(words(40_000, "task")),
        Message::assistant("ok"),
    ];
    let prompt = Message::user("continue");
    let configured = shaper(Some(WINDOW));
    let shaped = configured.shape(&history, 3).expect("over budget");
    let error = configured
        .window_overflow(&shaped.messages, &prompt)
        .expect("over the window");
    assert!(error.contains("does not fit the model's context window of 32768 tokens"));

    // Without a configured window it is the provider's call.
    assert_eq!(
        shaper(None).window_overflow(&shaped.messages, &prompt),
        None
    );
}

fn agent(model: &MockCompletionModel, shaper: &ContextShaper) -> rig_agent::agent::Agent {
    AgentBuilder::new(model.clone())
        .tool(MockAddTool)
        .add_hook(shaper.clone())
        .build()
}

/// The issue's case on rig's own loop: a 32k model and a 40k-token
/// earlier conversation. The turn's first request fits, carries the
/// task, and the turn completes.
#[tokio::test]
async fn the_first_request_of_a_new_turn_fits_and_the_turn_completes() {
    let history = conversation(40_000);
    let shaper = shaper(Some(WINDOW));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::text("done"),
        MockStreamEvent::final_response_with_total_tokens(4),
    ]]);
    let mut stream = agent(&model, &shaper)
        .prompt("continue the task")
        .history(history.clone())
        .max_turns(2)
        .stream();
    while let Some(item) = stream.next().await {
        item.expect("the turn completes");
    }

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let sent: &Vec<Message> = &requests[0]
        .chat_history
        .iter()
        .map(|m| (**m).clone())
        .collect();
    assert!(
        BASE + tokens(sent) <= WINDOW - WINDOW / 10 - RESPONSE_RESERVE,
        "{} tokens sent",
        BASE + tokens(sent)
    );
    assert_eq!(text_of(&sent[0]).as_deref(), Some(TASK));
    assert_eq!(
        text_of(sent.last().unwrap()).as_deref(),
        Some("continue the task")
    );
    assert!(tool_round_trips_intact(sent, &[]));
}

/// One that cannot fit ends the run with that error, and the model is
/// never called.
#[tokio::test]
async fn a_request_that_cannot_fit_stops_the_run_before_the_model() {
    let history = vec![
        Message::user(words(40_000, "task")),
        Message::assistant("ok"),
    ];
    let shaper = shaper(Some(WINDOW));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::text("done"),
        MockStreamEvent::final_response_with_total_tokens(4),
    ]]);
    let mut stream = agent(&model, &shaper)
        .prompt("continue")
        .history(history)
        .max_turns(2)
        .stream();
    let mut error = None;
    while let Some(item) = stream.next().await {
        if let Err(e) = item {
            error = Some(e.to_string());
        }
    }
    let error = error.expect("the run ends with an error");
    assert!(
        error.contains("does not fit the model's context window"),
        "{error}"
    );
    assert!(model.requests().is_empty(), "no request was sent");
}
