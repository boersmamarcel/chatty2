//! The hand-off against a fake server (HX-14): send (new and already
//! synced), continue here, a failure halfway, a 409, a stale version, and a
//! local-model conversation asking for a cloud model.

use std::sync::Arc;
use std::time::Duration;

use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

fn row(model_id: &str) -> ConversationData {
    ConversationData {
        id: "local-1".into(),
        title: "Plan the trip".into(),
        model_id: model_id.into(),
        message_history: r#"[{"role":"user","content":[{"type":"text","text":"hi"}]}]"#.into(),
        system_traces: "[null]".into(),
        token_usage: "{}".into(),
        attachment_paths: "[[]]".into(),
        message_timestamps: "[1]".into(),
        message_feedback: "[null]".into(),
        regeneration_records: "[]".into(),
        created_at: 1,
        updated_at: 2,
        working_dir: Some("/home/me/project".into()),
        agent_task_snapshot: None,
        sync: None,
        placement: None,
        remote_version: None,
        last_synced_at: None,
        tool_call_count: 3,
        context_tokens: 4,
    }
}

fn on_device(version: u64) -> ConversationSync {
    ConversationSync::Synced(SyncedConversation {
        server_url: "unused".into(),
        remote_id: "r-1".into(),
        version,
        placement: Placement::Device {
            device_id: "d-1".into(),
        },
        last_synced_at: Some(1),
    })
}

async fn roster(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/settings/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "cloud-a", "name": "Cloud A"},
            {"id": "cloud-b", "name": "Cloud B", "default": true}
        ])))
        .mount(server)
        .await;
}

async fn placement_ok(server: &MockServer, to: &str, version: u64) {
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(serde_json::json!({"to": to})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1",
            "version": version,
            "placement": to,
            "placement_device": if to == "device" {
                serde_json::json!({"id": "d-9", "name": "This Mac"})
            } else {
                serde_json::Value::Null
            }
        })))
        .mount(server)
        .await;
}

fn handoff(server: &MockServer) -> HandoffServer {
    HandoffServer::new(&format!("{}/", server.uri()), None)
}

fn body(request: &Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap()
}

#[tokio::test]
async fn a_new_conversation_is_created_with_its_client_id_then_placed_in_the_cloud() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .and(body_partial_json(
            serde_json::json!({"client_id": "local-1", "placement": "device"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 1, "placement": "device",
            "placement_device": {"id": "d-9", "name": "This Mac"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    placement_ok(&server, "cloud", 2).await;

    let local = row("cloud-a");
    let sync = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .expect("the hand-off succeeds");

    let synced = sync.synced().unwrap();
    assert_eq!(synced.remote_id, "r-1");
    assert_eq!(synced.version, 2);
    assert_eq!(synced.server_url, server.uri());
    assert_eq!(
        synced.placement,
        Placement::Cloud {
            model_id: Some("cloud-a".into())
        }
    );

    let requests = server.received_requests().await.unwrap();
    let create = body(
        requests
            .iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path() == "/api/conversations")
            .unwrap(),
    );
    // The full row goes up, but never the local working folder; and no
    // `messages` beside it, which the server refuses.
    assert_eq!(create["conversation"]["tool_call_count"], 3);
    assert!(create["conversation"]["working_dir"].is_null());
    assert!(create.get("messages").is_none());
    let placement = body(
        requests
            .iter()
            .find(|r| r.url.path().ends_with("/placement"))
            .unwrap(),
    );
    assert_eq!(
        placement,
        serde_json::json!({"to": "cloud", "expected_version": 1}),
        "the placement body carries nothing else: unknown fields are a 422"
    );
}

#[tokio::test]
async fn a_synced_conversation_pushes_its_history_with_if_match() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("PUT"))
        .and(path("/api/conversations/r-1/history"))
        .and(header("if-match", "3"))
        .and(body_partial_json(
            serde_json::json!({"id": "local-1", "model_id": "cloud-a"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "placement": "device",
            "placement_device": {"id": "d-1", "name": null}, "version": 4
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(
            serde_json::json!({"to": "cloud", "expected_version": 4}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": 5})))
        .expect(1)
        .mount(&server)
        .await;

    let local = row("cloud-a");
    let sync = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &on_device(3),
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap();
    assert!(sync.runs_in_cloud());
    assert_eq!(sync.synced().unwrap().version, 5);
}

/// Uploads rewrite the attachment references, which go up in one more push.
struct RewritingUploader;

impl AttachmentUploader for RewritingUploader {
    fn upload_attachments<'a>(
        &'a self,
        _server: &'a HandoffServer,
        remote_id: &'a str,
        mut conversation: ConversationData,
    ) -> BoxFuture<'a, anyhow::Result<ConversationData>> {
        Box::pin(async move {
            assert_eq!(remote_id, "r-1");
            conversation.attachment_paths = r#"[["hive-file:f-1"]]"#.into();
            Ok(conversation)
        })
    }
}

#[tokio::test]
async fn rewritten_attachment_references_are_pushed_before_the_placement_moves() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": "r-1", "version": 1})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/conversations/r-1/history"))
        .and(header("if-match", "1"))
        .and(body_partial_json(serde_json::json!({
            "attachment_paths": "[[\"hive-file:f-1\"]]"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": 2})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(
            serde_json::json!({"expected_version": 2}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let local = row("cloud-a");
    let sync = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: None,
        },
        &RewritingUploader,
    )
    .await
    .unwrap();
    assert_eq!(sync.synced().unwrap().version, 2);
}

/// AGE-298's rule, kept: a failure halfway — here the server accepted the
/// history but the placement never moved — leaves the local copy exactly as
/// it was, and so does a client killed mid-upload. The retry reuses the
/// `client_id`, so the server answers with the conversation it already made.
#[tokio::test]
async fn a_failure_halfway_or_a_kill_mid_upload_leaves_the_local_copy_untouched() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .and(body_partial_json(
            serde_json::json!({"client_id": "local-1"}),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": "r-1", "version": 1})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let local = row("cloud-a");
    let before = serde_json::to_string(&local).unwrap();
    let mut sync = ConversationSync::LocalOnly;

    let failed = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &sync,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await;
    assert!(matches!(failed, Err(HandoffError::Server(_))));
    assert_eq!(serde_json::to_string(&local).unwrap(), before);
    assert_eq!(sync, ConversationSync::LocalOnly);

    // Killed mid-upload: the placement call hangs and the future is dropped.
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let killed = tokio::time::timeout(
        Duration::from_millis(200),
        send_to_cloud(
            &handoff(&server),
            SendToCloud {
                conversation: &local,
                sync: &sync,
                cloud_model: None,
            },
            &NoAttachmentUpload,
        ),
    )
    .await;
    assert!(killed.is_err(), "the hand-off was cut off");
    assert_eq!(serde_json::to_string(&local).unwrap(), before);
    assert_eq!(sync, ConversationSync::LocalOnly);

    // The retry lands on the same remote conversation.
    placement_ok(&server, "cloud", 2).await;
    sync = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &sync,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap();
    assert_eq!(sync.synced().unwrap().remote_id, "r-1");
}

#[tokio::test]
async fn a_409_names_the_device_that_holds_it_and_continue_here_takes_it_over() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("PUT"))
        .and(path("/api/conversations/r-1/history"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "held_elsewhere",
            "message": "This conversation is running on Work laptop. Continue it here first.",
            "placement": "device",
            "placement_device": {"id": "d-2", "name": "Work laptop"},
            "version": 3
        })))
        .mount(&server)
        .await;

    let local = row("cloud-a");
    let error = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &on_device(3),
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, HandoffError::HeldElsewhere { .. }));
    // The server's own words, as-is.
    assert_eq!(
        error.to_string(),
        "This conversation is running on Work laptop. Continue it here first."
    );

    // Taking it over is continuing it here.
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 7, "turn_status": "idle", "model_id": "cloud-a",
            "messages": [], "placement": "device",
            "placement_device": {"id": "d-2", "name": "Work laptop"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(
            serde_json::json!({"to": "device", "expected_version": 7}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 8, "placement": "device",
            "placement_device": {"id": "d-1", "name": "This Mac"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let here = continue_here(&handoff(&server), &on_device(3), "d-1")
        .await
        .unwrap();
    assert_eq!(here.remote.version, Some(7));
    let synced = here.sync.synced().unwrap();
    assert_eq!(synced.version, 8);
    assert_eq!(
        synced.placement,
        Placement::Device {
            device_id: "d-1".into()
        }
    );
}

#[tokio::test]
async fn continue_here_pulls_the_cloud_copy_then_takes_the_placement() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 5, "turn_status": "idle", "model_id": "cloud-a",
            "messages": [{"role":"user","content":[{"type":"text","text":"hi"}]}]
        })))
        .mount(&server)
        .await;
    placement_ok(&server, "device", 6).await;

    let cloud = ConversationSync::cloud(server.uri(), "r-1", Some("cloud-a".into()));
    let here = continue_here(&handoff(&server), &cloud, "d-1")
        .await
        .unwrap();
    assert_eq!(here.remote.messages.len(), 1);
    assert!(!here.sync.runs_in_cloud());
    assert_eq!(here.sync.synced().unwrap().version, 6);
    assert_eq!(
        here.sync.placement(),
        Some(&Placement::Device {
            device_id: "d-1".into()
        })
    );
}

/// A save between the pull and the move raises the version: the placement
/// answers 412, and continuing pulls again and retries once.
#[tokio::test]
async fn continue_here_pulls_again_and_retries_once_after_a_412() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 5, "turn_status": "idle", "messages": []
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 6, "turn_status": "idle", "messages": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(
            serde_json::json!({"expected_version": 5}),
        ))
        .respond_with(
            ResponseTemplate::new(412)
                .set_body_json(serde_json::json!({"error": "version_mismatch", "version": 6})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .and(body_partial_json(
            serde_json::json!({"expected_version": 6}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 7, "placement": "device",
            "placement_device": {"id": "d-1", "name": null}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cloud = ConversationSync::cloud(server.uri(), "r-1", None);
    let here = continue_here(&handoff(&server), &cloud, "d-1")
        .await
        .unwrap();
    assert_eq!(here.remote.version, Some(6));
    assert_eq!(here.sync.synced().unwrap().version, 7);
}

/// This device's id is the login session the server marks `current`, never
/// whatever a placement reply names.
#[tokio::test]
async fn this_device_is_the_current_login_session() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/auth/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "s-other", "current": false},
            {"id": "s-me", "current": true}
        ])))
        .mount(&server)
        .await;
    assert_eq!(this_device_id(&handoff(&server)).await.unwrap(), "s-me");

    let unknown = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/auth/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&unknown)
        .await;
    assert!(this_device_id(&handoff(&unknown)).await.is_err());
    assert!(
        continue_here(&handoff(&unknown), &on_device(1), "")
            .await
            .is_err(),
        "no device id, no placement: never an empty one"
    );
}

#[tokio::test]
async fn continue_here_is_refused_while_the_cloud_turn_runs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/r-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "r-1", "version": 5, "turn_status": "running", "messages": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let cloud = ConversationSync::cloud(server.uri(), "r-1", None);
    assert!(matches!(
        continue_here(&handoff(&server), &cloud, "d-1").await,
        Err(HandoffError::TurnRunning)
    ));
}

#[tokio::test]
async fn a_placement_409_mid_turn_says_a_reply_is_being_written() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": "r-1", "version": 1})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "turn_active",
            "message": "A turn is running on this conversation. Try again when it finishes.",
            "turn_status": "running"
        })))
        .mount(&server)
        .await;
    let local = row("cloud-a");
    let error = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, HandoffError::TurnRunning));
}

/// HX-13: a `client_id` whose conversation was deleted on the account is
/// answered 410; the hand-off stops there with the server's words, and
/// nothing is placed.
#[tokio::test]
async fn a_conversation_deleted_on_the_account_is_not_sent_again() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .respond_with(ResponseTemplate::new(410).set_body_json(serde_json::json!({
            "error": "deleted", "message": "This conversation was deleted."
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/r-1/placement"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let local = row("cloud-a");
    let error = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, HandoffError::DeletedOnAccount { .. }));
    assert_eq!(error.to_string(), "This conversation was deleted.");
}

#[tokio::test]
async fn a_stale_version_says_to_get_the_latest_copy() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("PUT"))
        .and(path("/api/conversations/r-1/history"))
        .respond_with(ResponseTemplate::new(412).set_body_json(serde_json::json!({
            "error": "version_mismatch", "version": 9
        })))
        .mount(&server)
        .await;
    let local = row("cloud-a");
    let error = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &on_device(3),
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, HandoffError::ChangedElsewhere));
}

/// D7: a conversation on a model only this computer runs asks for a cloud
/// model, with the account's default preselected; nothing is written until
/// one is picked, and the pick is what the cloud copy runs on.
#[tokio::test]
async fn a_local_model_conversation_asks_for_a_cloud_model() {
    let server = MockServer::start().await;
    roster(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .and(body_partial_json(serde_json::json!({
            "model_id": "cloud-b", "placement": "device", "conversation": {"model_id": "cloud-b"}
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"id": "r-1", "version": 1})),
        )
        .expect(1)
        .mount(&server)
        .await;
    placement_ok(&server, "cloud", 2).await;

    let local = row("llama3.2:latest");
    let error = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: None,
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap_err();
    let HandoffError::NeedsCloudModel(roster) = error else {
        panic!("expected a request for a cloud model, got {error:?}")
    };
    assert_eq!(roster.preselected(&local.model_id).unwrap().id, "cloud-b");

    let sync = send_to_cloud(
        &handoff(&server),
        SendToCloud {
            conversation: &local,
            sync: &ConversationSync::LocalOnly,
            cloud_model: Some("cloud-b"),
        },
        &NoAttachmentUpload,
    )
    .await
    .unwrap();
    assert_eq!(sync.cloud_model_label().as_deref(), Some("cloud-b"));
    // The local row still names its own model.
    assert_eq!(local.model_id, "llama3.2:latest");
}

/// AGE-835: every request carries the Hive bearer; a 401 says to sign in.
#[tokio::test]
async fn requests_carry_the_hive_bearer_and_a_401_says_to_sign_in() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/settings/models"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let hive = Arc::new(HiveSession::new(
        server.uri(),
        Some(hive_client::TokenPair {
            token: "tok".into(),
            refresh_token: "r".into(),
            expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
        }),
    ));
    let models = cloud_models(&HandoffServer::new(&server.uri(), Some(hive)))
        .await
        .unwrap();
    assert!(models.models.is_empty());

    let locked = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&locked)
        .await;
    let error = cloud_models(&handoff(&locked)).await.unwrap_err();
    assert_eq!(error.to_string(), SIGN_IN_TO_HIVE);
}

#[test]
fn a_hand_off_is_refused_mid_turn_and_to_where_it_already_is() {
    let local = ConversationSync::LocalOnly;
    let cloud = ConversationSync::cloud("http://localhost:8081", "r-1", None);
    assert!(matches!(
        refuse_reason(true, &local, true),
        Some(HandoffError::TurnRunning)
    ));
    assert!(matches!(
        refuse_reason(true, &cloud, false),
        Some(HandoffError::TurnRunning)
    ));
    assert!(matches!(
        refuse_reason(false, &cloud, true),
        Some(HandoffError::AlreadyThere { in_cloud: true })
    ));
    assert!(matches!(
        refuse_reason(false, &on_device(1), false),
        Some(HandoffError::AlreadyThere { in_cloud: false })
    ));
    assert!(matches!(
        refuse_reason(false, &local, false),
        Some(HandoffError::NotSynced)
    ));
    assert!(refuse_reason(false, &local, true).is_none());
    assert!(refuse_reason(false, &on_device(1), true).is_none());
    assert!(refuse_reason(false, &cloud, false).is_none());
}

#[test]
fn the_local_model_is_the_clouds_when_configured_else_the_conversations_else_the_default() {
    let configured = |model: &str| model == "here-a" || model == "here-b";
    assert_eq!(
        pick_local_model("here-a", "here-b", None, configured).as_deref(),
        Some("here-a")
    );
    assert_eq!(
        pick_local_model("cloud-x", "here-b", Some("here-a"), configured).as_deref(),
        Some("here-b")
    );
    assert_eq!(
        pick_local_model("cloud-x", "gone", Some("here-a"), configured).as_deref(),
        Some("here-a")
    );
    assert_eq!(pick_local_model("cloud-x", "gone", None, configured), None);
}

/// HX-16: the dialog says what goes and what stays, in plain words, and
/// that provider keys never leave.
#[test]
fn the_summary_says_what_goes_and_what_stays() {
    for item in ["Messages", "Tool activity", "Usage", "Attachments"] {
        assert!(SEND_TO_CLOUD_SUMMARY.goes.contains(&item), "{item}");
    }
    for item in [
        "Files in your working folder",
        "Local MCP servers",
        "Your provider keys",
        "Memory",
        "Skills",
    ] {
        assert!(SEND_TO_CLOUD_SUMMARY.stays.contains(&item), "{item}");
    }
    assert!(
        CONTINUE_HERE_SUMMARY
            .lead
            .contains("cloud copy stays in your Hive account")
    );
    let all = [
        SEND_TO_CLOUD_SUMMARY.lead,
        CONTINUE_HERE_SUMMARY.lead,
        LOCAL_MODEL_NOTE,
    ]
    .join(" ")
    .to_lowercase();
    for internal in ["placement", "sync state", "chatty-server", "hosted"] {
        assert!(!all.contains(internal), "internal word {internal:?}");
    }
}
