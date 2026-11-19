use super::*;
use crate::services::context_compaction::COMPACTION_HEADER;
use futures::StreamExt;
use rig_agent::agent::AgentBuilder;
use rig_agent::test_utils::MockAddTool;
use rig_core::completion::message::AssistantContent;
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, MockTurn};

const TASK: &str = "Fix the off-by-one in parser.py so test_parse_range passes.";

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

fn call(id: &str, name: &str, args: serde_json::Value) -> Message {
    Message::Assistant(rig_core::message::AssistantMessage::new(vec![
        AssistantContent::tool_call(id, crate::testing::tool_name(name), args),
    ]))
}

fn result(id: &str, name: &str, text: String) -> Message {
    Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            call: CallId::from_wire(id),
            name: crate::testing::tool_name(name),
            content: vec![ToolResultContent::Text(Text::new(text))],
            is_error: false,
        })],
    }
}

/// The task, then `n` round-trips of a coding run: reads, one file
/// write, and test runs whose output ends on a recognisable line.
fn coding_run(n: usize) -> Vec<Message> {
    let mut history = vec![Message::user(TASK)];
    for i in 0..n {
        let id = format!("c{i}");
        let (name, args, output) = match i % 4 {
            0 => (
                "read_file",
                serde_json::json!({"path": format!("src/mod{i}.py")}),
                vec!["line"; 120].join(" "),
            ),
            1 if i == 1 => (
                "write_file",
                serde_json::json!({"path": "parser.py", "content": "..."}),
                "written".to_string(),
            ),
            _ => (
                "shell_execute",
                serde_json::json!({"command": format!("pytest -q tests/test_{i}.py")}),
                format!(
                    "{} FAILED test_parse_range run {i}",
                    vec!["dot"; 120].join(" ")
                ),
            ),
        };
        history.push(call(&id, name, args));
        history.push(result(&id, name, output));
    }
    history
}

fn tokens(history: &[Message]) -> usize {
    let counter = counter();
    history.iter().map(|m| counter.count_message(m)).sum()
}

fn text_of(message: &Message) -> String {
    match message {
        Message::User { content } => match content.first() {
            Some(UserContent::Text(t)) => t.text.clone(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected a user message, got {other:?}"),
    }
}

fn assert_round_trips_intact(messages: &[Message], answered_outside: &[CallId]) {
    assert!(
        tool_round_trips_intact(messages, answered_outside),
        "a tool round-trip was split"
    );
}

/// A shaper whose budget puts `history` at `share` of it.
fn shaper_at(history: &[Message], share: f64) -> ContextShaper {
    let window = (tokens(history) as f64 / share) as usize;
    ContextShaper::new(settings(), counter(), Some(window))
}

fn summarizer(model: &MockCompletionModel) -> Arc<dyn Summarizer> {
    Arc::new(AgentBuilder::new(model.clone()).build())
}

const PROSE: &str = "Done so far: located the range check in parser.py.\n\
    Findings: the end index is exclusive.\nOpen questions: none.\n\
    Next step: rerun test_parse_range.";

#[tokio::test]
async fn below_the_threshold_nothing_is_compacted() {
    let history = coding_run(12);
    let shaper = shaper_at(&history, 0.80);
    let model = MockCompletionModel::from_turns([MockTurn::text(PROSE)]);
    shaper.enable_compaction(Some(summarizer(&model)), None);

    let prompt = Message::user("go on");
    assert_eq!(shaper.prepare_request(&history, &prompt, 1).await, None);
    assert_eq!(model.request_count(), 0, "no summary call");
}

#[tokio::test]
async fn over_the_threshold_the_old_part_becomes_a_grounded_summary() {
    let history = coding_run(24);
    let shaper = shaper_at(&history, 0.95);
    let model =
        MockCompletionModel::from_turns([MockTurn::text(format!("<think>x</think>{PROSE}"))]);
    shaper.enable_compaction(Some(summarizer(&model)), None);

    let sent = shaper
        .prepare_request(&history, &Message::user("go on"), 1)
        .await
        .expect("compacted");

    assert_eq!(model.request_count(), 1, "one summary call");
    let summary = text_of(&sent[0]);
    assert!(summary.starts_with(COMPACTION_HEADER));
    assert!(summary.contains(TASK), "the task, verbatim");
    assert!(summary.contains("- parser.py (write_file)"), "{summary}");
    assert!(summary.contains("## Last command result"));
    assert!(summary.contains("FAILED test_parse_range run"));
    assert!(
        summary.contains(PROSE),
        "the model's prose, think block dropped"
    );
    assert!(!summary.contains("<think>"));
    assert!(
        summary.contains("re-check the current state first"),
        "re-grounding step"
    );

    // The newest messages go out verbatim, and nothing else follows them.
    let kept = sent.len() - 1;
    assert!(kept >= 2 && kept < history.len() / 2, "kept {kept}");
    assert_eq!(&sent[1..], &history[history.len() - kept..]);
    assert!(matches!(sent[1], Message::Assistant(_)), "starts on a call");
    assert_round_trips_intact(&sent, &[]);
    assert!(tokens(&sent) < tokens(&history) * 4 / 5);
}

#[tokio::test]
async fn a_failed_summary_call_falls_back_to_a_deterministic_summary() {
    let history = coding_run(24);
    let shaper = shaper_at(&history, 0.95);
    let model = MockCompletionModel::from_turns([MockTurn::error("provider down")]);
    shaper.enable_compaction(Some(summarizer(&model)), None);

    let sent = shaper
        .prepare_request(&history, &Message::user("go on"), 1)
        .await
        .expect("compacted");

    let summary = text_of(&sent[0]);
    assert!(summary.contains("Model summary unavailable"));
    assert!(summary.contains(TASK));
    assert!(summary.contains("- parser.py (write_file)"));
    assert!(summary.contains("FAILED test_parse_range run"));
    assert!(summary.contains("shell_execute ×"));
    assert_round_trips_intact(&sent, &[]);
}

/// Compaction is a discrete event: the requests after it reuse the same
/// summary, so their prefix is stable, and no further model call is made
/// until the history grows past the trigger again.
#[tokio::test]
async fn later_requests_reuse_the_compaction() {
    let mut history = coding_run(24);
    let shaper = shaper_at(&history, 0.95);
    let model = MockCompletionModel::from_turns([MockTurn::text(PROSE)]);
    shaper.enable_compaction(Some(summarizer(&model)), None);
    let first = shaper
        .prepare_request(&history, &Message::user("go on"), 1)
        .await
        .expect("compacted");

    history.push(call("next", "read_file", serde_json::json!({"path": "a"})));
    let prompt = result("next", "read_file", "short".into());
    let second = shaper
        .prepare_request(&history, &prompt, 2)
        .await
        .expect("the compaction still applies");

    assert_eq!(model.request_count(), 1, "no second summary call");
    assert_eq!(second[..first.len()], first[..], "same prefix");
    assert_eq!(second.last(), history.last());
    assert_round_trips_intact(&second, &[CallId::from_wire("next")]);
}

#[tokio::test]
async fn the_summary_carries_a_git_status_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !git(&["init", "-q"]) {
        return; // no git on this host
    }
    std::fs::write(dir.path().join("parser.py"), "fixed").unwrap();

    let history = coding_run(24);
    let shaper = shaper_at(&history, 0.95);
    shaper.enable_compaction(None, Some(dir.path().to_path_buf()));
    let sent = shaper
        .prepare_request(&history, &Message::user("go on"), 1)
        .await
        .expect("compacted");

    let summary = text_of(&sent[0]);
    assert!(summary.contains("`git status --short` now:\n```\n?? parser.py\n```"));
    assert!(summary.contains("`git status && git diff`"));
}

/// On rig's own loop with scripted streams: the request the model gets
/// carries the summary, then the recent messages, then the prompt.
#[tokio::test]
async fn the_hook_sends_the_compacted_history_to_the_model() {
    let history = coding_run(24);
    let prompt_text = "continue";
    let counter = counter();
    let total = tokens(&history) + counter.count_message(&Message::user(prompt_text));
    let shaper = ContextShaper::new(settings(), counter, Some((total as f64 / 0.95) as usize));
    let utility = MockCompletionModel::from_turns([MockTurn::text(PROSE)]);
    shaper.enable_compaction(Some(summarizer(&utility)), None);

    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::text("done"),
        MockStreamEvent::final_response_with_total_tokens(4),
    ]]);
    let agent = AgentBuilder::new(model.clone())
        .tool(MockAddTool)
        .add_hook(shaper.clone())
        .build();
    let mut stream = agent
        .prompt(prompt_text)
        .history(history.clone())
        .max_turns(2)
        .stream();
    while let Some(item) = stream.next().await {
        item.expect("the run succeeds");
    }

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let sent: &Vec<Message> = &requests[0]
        .chat_history
        .iter()
        .map(|m| (**m).clone())
        .collect();
    assert!(text_of(&sent[0]).starts_with(COMPACTION_HEADER));
    assert!(sent.len() < history.len());
    // rig 0.46 joins adjacent user messages (AGE-1151): the prompt
    // closes the message that holds the last tool result.
    let Message::User { content } = sent.last().unwrap() else {
        panic!("the request ends on a user message")
    };
    assert!(
        matches!(content.last(), Some(UserContent::Text(t)) if t.text == prompt_text),
        "{content:?}"
    );
    assert_round_trips_intact(sent, &[]);
    assert_eq!(utility.request_count(), 1);
}
