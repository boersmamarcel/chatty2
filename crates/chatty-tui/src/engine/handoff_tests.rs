//! HX-19: the terminal's hand-off commands and the held-elsewhere display,
//! against the same stand-in cloud server as `cloud_tests`.

use std::time::Duration;

use chatty_core::models::conversation::Placement;
use chatty_core::repositories::ConversationRepository;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::cloud_tests::{cloud, detail, serve_list, system_lines};

#[tokio::test]
async fn send_to_cloud_shows_what_leaves_first_and_sends_only_on_confirm() {
    let mut cloud = cloud().await;
    Mock::given(method("GET"))
        .and(path("/api/settings/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "m1", "name": "Model one", "default": true}
        ])))
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({"id": "r-1", "version": 1, "messages": []})),
        )
        .expect(1)
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(json!({"to": "cloud"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 2, "placement": {"kind": "cloud"}
        })))
        .expect(1)
        .mount(&cloud.server)
        .await;

    cloud.engine.send_to_cloud_command(None).await;
    let shown = system_lines(&cloud.engine);
    assert!(shown.contains("Goes with it"), "{shown}");
    assert!(shown.contains("Stays on this computer"), "{shown}");
    assert!(shown.contains("/send-to-cloud confirm"), "{shown}");
    assert!(cloud.engine.hosted.is_none(), "nothing was sent yet");

    cloud.engine.send_to_cloud_command(Some("confirm")).await;

    assert!(system_lines(&cloud.engine).contains("now runs in the cloud"));
    assert!(cloud.engine.hosted.is_some());
    assert!(cloud.engine.conversation_sync().runs_in_cloud());
    // The cloud conversation is saved, so it is in the list next time.
    let mut saved = None;
    for _ in 0..40 {
        saved = cloud.repo.load_one("c1").await.unwrap();
        if saved.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(saved.expect("saved").sync.is_some());
}

#[tokio::test]
async fn a_model_the_cloud_does_not_run_asks_for_one_and_changes_nothing() {
    let mut cloud = cloud().await;
    Mock::given(method("GET"))
        .and(path("/api/settings/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "cloud-a", "name": "Cloud A"}
        ])))
        .mount(&cloud.server)
        .await;

    cloud.engine.send_to_cloud_command(Some("confirm")).await;

    let shown = system_lines(&cloud.engine);
    assert!(shown.contains("only runs on this computer"), "{shown}");
    assert!(shown.contains("cloud-a  Cloud A"), "{shown}");
    assert!(
        shown.contains("/send-to-cloud confirm <model id>"),
        "{shown}"
    );
    assert!(cloud.engine.hosted.is_none());
    assert!(!cloud.engine.conversation_sync().runs_in_cloud());
}

#[tokio::test]
async fn continue_here_brings_a_cloud_conversation_back_to_this_device() {
    let mut cloud = cloud().await;
    serve_list(&cloud.server, "idle").await;
    cloud.engine.conversations_command().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(detail(
            "idle",
            &[
                ("user", "Hi"),
                ("assistant", "Hello"),
                ("assistant", "While you were away"),
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
        .and(path("/api/auth/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "other", "current": false},
            {"id": "this-dev", "current": true}
        ])))
        .expect(1)
        .mount(&cloud.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(json!({"to": "device"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 4, "placement": {"kind": "device", "device_id": "d-9"}
        })))
        .expect(1)
        .mount(&cloud.server)
        .await;
    cloud.engine.open_command("1").await;
    assert!(cloud.engine.hosted.is_some());

    cloud.engine.continue_here_command().await;

    let shown = system_lines(&cloud.engine);
    assert!(shown.contains("now runs on this computer"), "{shown}");
    assert!(shown.contains("While you were away"), "{shown}");
    assert!(cloud.engine.hosted.is_none(), "turns run here again");
    let sync = cloud.engine.conversation_sync();
    assert_eq!(
        sync.placement(),
        Some(&Placement::Device {
            device_id: "this-dev".into()
        })
    );
}

#[tokio::test]
async fn a_conversation_another_device_holds_says_which() {
    let mut cloud = cloud().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "conversations": [
                {"id": "r-3", "title": "On the laptop", "version": 2, "updated_at_ms": 3000,
                 "turn_status": "idle",
                 "placement": {"kind": "device", "device_id": "work"},
                 "placement_device": {"id": "work", "name": "Work laptop"}},
                {"id": "r-4", "title": "Here", "version": 1, "updated_at_ms": 2000,
                 "turn_status": "idle",
                 "placement": {"kind": "device", "device_id": "this"},
                 "placement_device": {"id": "this", "name": "This one"}}
            ],
            "next_cursor": "2.0"
        })))
        .mount(&cloud.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/auth/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "other", "current": false},
            {"id": "this", "current": true}
        ])))
        .mount(&cloud.server)
        .await;

    let list = cloud.engine.conversations_command().await;

    assert!(list.contains("On the laptop  [on Work laptop]"), "{list}");
    assert!(list.contains("Here  [synced]"), "{list}");
}
