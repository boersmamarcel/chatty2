//! HX-19's characterization: the terminal against a stand-in cloud server.
//! One scenario per thing the issue names — list, open with reattach, send to
//! a cloud conversation, answer what it waits on — each run the way the main
//! loop does, feeding every event back through `handle_event`.

use std::sync::Arc;
use std::time::Duration;

use chatty_core::repositories::{ConversationRepository, ConversationSqliteRepository};
use chatty_core::services::hive_sync::HttpHiveConversationsApi;
use chatty_core::session::SessionEvent;
use rig_core::message::Message;
use serde_json::json;
use tokio::sync::mpsc;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::tests::test_engine;
use super::*;
use chatty_core::services::hive_sync::{ALREADY_ANSWERED, NO_LONGER_EXISTS};

pub(super) struct Cloud {
    pub(super) server: MockServer,
    pub(super) repo: Arc<ConversationSqliteRepository>,
    pub(super) engine: ChatEngine,
    pub(super) events: mpsc::UnboundedReceiver<AppEvent>,
    _dir: tempfile::TempDir,
}

pub(super) async fn cloud() -> Cloud {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(ConversationSqliteRepository::deferred_with_path(
        dir.path().join("conversations.db"),
    ));
    let (mut engine, events) = test_engine().await;
    engine.use_cloud(
        repo.clone(),
        Arc::new(HttpHiveConversationsApi::new(&server.uri(), None)),
        &server.uri(),
    );
    Cloud {
        server,
        repo,
        engine,
        events,
        _dir: dir,
    }
}

pub(super) fn history(texts: &[(&str, &str)]) -> serde_json::Value {
    let messages: Vec<Message> = texts
        .iter()
        .map(|(role, text)| match *role {
            "user" => Message::user(*text),
            _ => Message::assistant(*text),
        })
        .collect();
    serde_json::to_value(messages).unwrap()
}

pub(super) fn detail(status: &str, texts: &[(&str, &str)]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "r-1",
        "title": "Cloud research",
        "version": 3,
        "turn_status": status,
        "placement": "cloud",
        "messages": history(texts),
    }))
}

pub(super) async fn serve_list(server: &MockServer, status: &str) {
    Mock::given(method("GET"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "conversations": [
                {"id": "r-1", "title": "Cloud research", "version": 3,
                 "updated_at_ms": 2000, "turn_status": status, "placement": "cloud"},
                {"id": "r-2", "title": "Needs an answer", "version": 1,
                 "updated_at_ms": 1000, "turn_status": "waiting_for_you", "placement": "cloud"}
            ],
            "next_cursor": "2.0"
        })))
        .mount(server)
        .await;
}

pub(super) fn sse_session(event: &SessionEvent) -> String {
    format!(
        "event: session\ndata: {}\n\n",
        serde_json::to_string(event).unwrap()
    )
}

pub(super) fn sse(parts: &[String]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(parts.concat(), "text/event-stream")
}

/// Feed events through `handle_event` until `done` holds, or fail.
pub(super) async fn pump(cloud: &mut Cloud, done: impl Fn(&ChatEngine) -> bool) {
    for _ in 0..200 {
        if done(&cloud.engine) {
            return;
        }
        if let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(50), cloud.events.recv()).await
        {
            cloud.engine.handle_event(event);
        }
    }
    panic!("the engine never reached the expected state");
}

pub(super) fn transcript_text(engine: &ChatEngine) -> String {
    engine
        .transcript
        .messages
        .iter()
        .map(|message| message.text())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn system_lines(engine: &ChatEngine) -> String {
    transcript_text(engine)
}

#[tokio::test]
async fn conversations_lists_the_account_with_where_each_runs_and_what_its_turn_does() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "running").await;

    let list = cloud.engine.conversations_command().await;

    assert_eq!(
        list,
        "Conversations (/open <number> opens one):\n\
         \x20  1. Cloud research  [cloud]  Running in the cloud\n\
         \x20  2. Needs an answer  [cloud]  Waiting for you"
    );
    // The rows are kept, so the terminal has them next time it starts.
    let saved = cloud.repo.load_metadata().await.unwrap();
    assert_eq!(saved.len(), 2);
    assert!(saved.iter().all(|row| row.sync.runs_in_cloud()));
}

#[tokio::test]
async fn open_reads_a_stale_conversation_then_follows_its_running_turn() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "running").await;
    cloud.engine.conversations_command().await;

    // First read (on open): the turn is running. Second (when it ends): done.
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(detail(
            "running",
            &[("user", "Research this"), ("assistant", "On it")],
        ))
        .up_to_n_times(1)
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(detail(
            "idle",
            &[
                ("user", "Research this"),
                ("assistant", "On it"),
                ("assistant", "Here is the report"),
            ],
        ))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/inputs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/turn/events"))
        .respond_with(sse(&[
            "event: turn_attached\ndata: {\"started_at_ms\":1,\"dropped\":0}\n\n".to_string(),
            sse_session(&SessionEvent::TurnStarted),
            sse_session(&SessionEvent::Text("Writing the report".into())),
            sse_session(&SessionEvent::TurnEnded),
        ]))
        .mount(&cloud.server)
        .await;

    cloud.engine.open_command("1").await;

    // Opened: the server's history is on screen and the turn is followed.
    assert_eq!(cloud.engine.title, "Cloud research");
    assert!(transcript_text(&cloud.engine).contains("Research this"));
    assert!(
        cloud.engine.is_streaming,
        "the running turn is being followed"
    );
    assert!(cloud.engine.hosted.is_some());

    // The followed text is drawn; at the end the server's copy replaces it.
    pump(&mut cloud, |engine| {
        !engine.is_streaming && transcript_text(engine).contains("Here is the report")
    })
    .await;
    let conversation = cloud.engine.session.conversation().unwrap();
    assert_eq!(
        conversation.messages().len(),
        3,
        "the server's copy was adopted"
    );
    let saved = cloud.repo.load_one("r-1").await.unwrap().expect("saved");
    assert!(saved.last_synced_at.is_some(), "the copy is fresh");
    assert_eq!(saved.title, "Cloud research");
}

#[tokio::test]
async fn open_says_so_when_the_server_cannot_be_read() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "idle").await;
    cloud.engine.conversations_command().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/inputs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&cloud.server)
        .await;

    cloud.engine.open_command("1").await;

    assert!(system_lines(&cloud.engine).contains(NO_LONGER_EXISTS));
    assert!(!cloud.engine.is_streaming);
}

#[tokio::test]
async fn a_message_to_an_open_cloud_conversation_runs_on_the_server_and_is_kept() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "idle").await;
    cloud.engine.conversations_command().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(detail("idle", &[("user", "Hi"), ("assistant", "Hello")]))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/inputs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/turns"))
        .and(body_json(json!({"text": "And now?", "kind": "human"})))
        .respond_with(sse(&[
            sse_session(&SessionEvent::TurnStarted),
            sse_session(&SessionEvent::Text("Now this".into())),
            sse_session(&SessionEvent::TurnEnded),
        ]))
        .expect(1)
        .mount(&cloud.server)
        .await;
    cloud.engine.open_command("1").await;
    assert!(!cloud.engine.is_streaming);

    cloud.engine.send_message("And now?".to_string());
    pump(&mut cloud, |engine| {
        !engine.is_streaming && transcript_text(engine).contains("Now this")
    })
    .await;

    // The local copy records the turn, and it is saved for the next start.
    let mut kept = 0;
    for _ in 0..40 {
        let saved = cloud.repo.load_one("r-1").await.unwrap().expect("saved");
        let messages: Vec<Message> = serde_json::from_str(&saved.message_history).unwrap();
        kept = messages.len();
        if kept == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(kept, 4, "the turn was saved with the history");
}

#[tokio::test]
async fn what_a_cloud_run_waits_on_is_answered_with_the_existing_prompts() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "waiting_for_you").await;
    cloud.engine.conversations_command().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(detail("idle", &[("user", "Hi"), ("assistant", "Hello")]))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/inputs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "approval-1", "nonce": "n1",
             "request": {"approval": {"kind": "exec", "command_or_path": "cargo test"}}},
            {"id": "question-1", "nonce": "n2",
             "request": {"question": {"questions": [
                 {"id": "q1", "question": "Which branch?", "options": ["main", "dev"]}]}}}
        ])))
        .mount(&cloud.server)
        .await;
    // The turn the run is waiting in is followed too; it has nothing to say.
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1/turn/events"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/inputs/approval-1"))
        .and(body_json(json!({"nonce": "n1", "approved": true})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/inputs/question-1"))
        .and(body_json(json!({
            "nonce": "n2",
            "answers": [{"id": "q1", "answer": "dev", "custom": false}]
        })))
        .respond_with(ResponseTemplate::new(409))
        .expect(1)
        .mount(&cloud.server)
        .await;

    cloud.engine.open_command("1").await;
    let approval = cloud
        .engine
        .pending_approval
        .clone()
        .expect("an approval card");
    assert_eq!(approval.command, "cargo test");
    assert!(cloud.engine.pending_clarification.is_some());

    cloud.engine.approve();
    assert!(cloud.engine.pending_approval.is_none());
    cloud.engine.answer_clarification_option(1);
    assert!(cloud.engine.pending_clarification.is_none());

    // The second answer was settled elsewhere already: say so.
    pump(&mut cloud, |engine| {
        system_lines(engine).contains(ALREADY_ANSWERED)
    })
    .await;
    // `expect(1)` on both mocks is checked when the server drops.
}
