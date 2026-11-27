//! A provider that refuses images (AGE-2178): vLLM serving a model with
//! `--limit-mm-per-prompt '{"image":0}'` answers any request carrying one
//! with `400 At most 0 image(s) may be provided in one prompt.` The turn
//! sends the request again without its images and carries on, the agent
//! strips images up front from then on, and a 4xx is never retried as it
//! was sent.

use std::cell::RefCell;
use std::rc::Rc;

use rig_core::completion::message::{DocumentSourceKind, Image, ImageMediaType};

use super::*;
use crate::agent_spec::AgentSpec;
use crate::services::{RecoveryAction, StreamSurface};
use crate::settings::models::providers_store::ProviderType;
use crate::testing::fake_model::{FakeDaemon, Reply, Script};

const MODEL: &str = "age-2178-model";

/// What vLLM answers a request with an image when images are off.
fn image_refusal() -> Reply {
    Reply::ErrorBody(
        400,
        serde_json::json!({ "error": {
            "message": "At most 0 image(s) may be provided in one prompt.",
            "type": "BadRequestError", "param": null, "code": 400
        }}),
    )
}

/// A headless session on the fake model, whose config wrongly says the
/// model takes images (OpenRouter's blanket default).
async fn session_on(daemon: &FakeDaemon) -> (AgentSession, tempfile::TempDir) {
    let _ = crate::init_repositories();
    let workspace = tempfile::tempdir().expect("a workspace");
    let settings = ExecutionSettingsModel {
        workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
        fetch_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let mut model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::OpenRouter,
        MODEL.to_string(),
    );
    model.supports_images = true;
    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::OpenRouter)
        .with_api_key("test-key".to_string())
        .with_base_url(daemon.base_url());
    let built = AgentBuildContext::from_spec(
        &AgentSpec::named("worker"),
        crate::factories::agent_factory::AgentServices {
            exec_settings: Some(settings.clone()),
            ..Default::default()
        },
    )
    .expect("the spec builds");
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings,
        surface: StreamSurface::Headless,
        loop_guard: false,
    });
    session
        .create_conversation(
            "age-2178".to_string(),
            "Image task".to_string(),
            &model,
            &provider,
            built.context,
        )
        .await
        .expect("the conversation builds");
    (session, workspace)
}

/// Run `input` as one turn, apply its events and finish it.
async fn run_turn(session: &mut AgentSession, input: TurnInput) -> Vec<SessionEvent> {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    session
        .begin_turn(input, move |event| sink.borrow_mut().push(event))
        .expect("the turn starts")
        .await;
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    session.finish_turn(None, vec![]);
    events
}

fn with_image(text: &str) -> TurnInput {
    let mut input = TurnInput::text(text);
    input.contents.push(UserContent::Image(Image {
        data: DocumentSourceKind::Base64("iVBORw0KGgo=".to_string()),
        media_type: Some(ImageMediaType::PNG),
        ..Default::default()
    }));
    input
}

fn errors(events: &[SessionEvent]) -> Vec<StreamError> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::Error(error) => Some(error.clone()),
            _ => None,
        })
        .collect()
}

fn bodies(daemon: &FakeDaemon) -> Vec<String> {
    daemon
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

/// The refusal is answered once, without the image, and the turn ends with
/// the model's answer; the next turn sends no image at all.
#[tokio::test]
async fn an_image_refusal_drops_the_image_and_the_turn_answers() {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            image_refusal(),
            Reply::text("I cannot see the image."),
            Reply::text("Still no image."),
        ],
    ));
    let (mut session, _workspace) = session_on(&daemon).await;

    let events = run_turn(&mut session, with_image("What does the picture show?")).await;

    assert!(errors(&events).is_empty(), "{:?}", errors(&events));
    let reply = session
        .conversation()
        .and_then(|c| c.messages().last().cloned())
        .expect("the turn was kept");
    assert!(
        format!("{reply:?}").contains("I cannot see the image."),
        "{reply:?}"
    );
    let sent = bodies(&daemon);
    assert_eq!(
        sent.len(),
        2,
        "the refused request, then one without images"
    );
    assert!(sent[0].contains("image_url"), "{}", sent[0]);
    assert!(!sent[1].contains("image_url"), "{}", sent[1]);
    assert!(
        sent[1].contains("Image omitted"),
        "the model is told the image was dropped: {}",
        sent[1]
    );
    assert!(sent[1].contains("What does the picture show?"));

    // The agent is text-only now: the next turn strips the history's image
    // up front instead of meeting the refusal again.
    let events = run_turn(&mut session, TurnInput::text("And now?")).await;
    assert!(errors(&events).is_empty(), "{:?}", errors(&events));
    let sent = bodies(&daemon);
    assert_eq!(sent.len(), 3, "one request, not refused");
    assert!(!sent[2].contains("image_url"), "{}", sent[2]);
}

/// A 400 that is not about images ends the turn after one request, and
/// headless does not send it again.
#[tokio::test]
async fn another_400_is_sent_once_and_not_retried() {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [Reply::ErrorBody(
            400,
            serde_json::json!({ "error": { "message": "Invalid tool schema.", "code": 400 } }),
        )],
    ));
    let (mut session, _workspace) = session_on(&daemon).await;

    let events = run_turn(&mut session, with_image("What does the picture show?")).await;

    let errors = errors(&events);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].kind, StreamErrorKind::ProviderStatus(400));
    assert_eq!(daemon.requests().len(), 1);
    assert_eq!(session.recovery_action(&errors[0]), RecoveryAction::Stop);
}
