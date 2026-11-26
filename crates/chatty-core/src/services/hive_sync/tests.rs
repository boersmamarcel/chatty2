//! The sync engine driven against the fake server, step by step, with the
//! resulting rows pinned in a golden (AGE-2088).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::*;
use crate::models::conversation::{Conversation, Placement, SyncedConversation};
use crate::repositories::ConversationSqliteRepository;

const SERVER: &str = "https://hive.example";

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/hive_sync/goldens/sync_engine.txt")
}

fn local_only(id: &str, title: &str, updated_at: i64) -> ConversationMetadata {
    ConversationMetadata {
        id: id.to_string(),
        title: title.to_string(),
        total_cost: 0.5,
        updated_at,
        sync: ConversationSync::LocalOnly,
        tool_call_count: 3,
        context_tokens: 1_000,
    }
}

fn render_sync(sync: &ConversationSync) -> String {
    match sync {
        ConversationSync::LocalOnly => "local-only".to_string(),
        ConversationSync::Synced(SyncedConversation {
            server_url,
            remote_id,
            version,
            placement,
            last_synced_at,
        }) => {
            let placement = match placement {
                Placement::Cloud { .. } => "cloud".to_string(),
                Placement::Device { device_id } => format!("device:{device_id}"),
            };
            let detail = match last_synced_at {
                Some(at) => format!("synced@{at}"),
                None => "stale".to_string(),
            };
            format!("synced {server_url} {remote_id} v{version} {placement} {detail}")
        }
    }
}

fn render_action(action: &SyncAction) -> String {
    match action {
        SyncAction::UpsertMetadata(row) => format!(
            "upsert {} \"{}\" t={} {} turn={:?}",
            row.local_id,
            row.title,
            row.updated_at,
            render_sync(&row.sync),
            row.turn_status
        ),
        SyncAction::ApplyTombstone { local_id } => format!("tombstone {local_id}"),
        SyncAction::MarkDetailStale { local_id } => format!("stale {local_id}"),
        SyncAction::FullResync => "full-resync".to_string(),
    }
}

fn render_rows(rows: &[ConversationMetadata]) -> String {
    rows.iter()
        .map(|row| {
            format!(
                "  {} \"{}\" t={} {}\n",
                row.id,
                row.title,
                row.updated_at,
                render_sync(&row.sync)
            )
        })
        .collect()
}

/// One step: pull, apply to the in-memory rows *and* to a real SQLite
/// repository, check the two agree, and record both the actions and the
/// rows.
async fn step(
    log: &mut String,
    name: &str,
    api: &FakeHiveConversations,
    engine: &mut SyncEngine,
    rows: &mut Vec<ConversationMetadata>,
    repo: &ConversationSqliteRepository,
) {
    // Two changes a page, so the steps page.
    let actions = pull_pages(api, engine, rows, 2)
        .await
        .expect("the pull succeeds");
    apply_to_rows(rows, &actions);
    apply_to_repository(repo, &actions)
        .await
        .expect("the actions apply to the repository");

    let stored = repo.load_metadata().await.expect("reload");
    let view = |rows: &[ConversationMetadata]| -> Vec<(String, String, i64, ConversationSync)> {
        rows.iter()
            .map(|r| (r.id.clone(), r.title.clone(), r.updated_at, r.sync.clone()))
            .collect()
    };
    assert_eq!(
        view(&stored),
        view(rows),
        "{name}: the repository and the sidebar rows must agree"
    );

    writeln!(log, "## {name}").unwrap();
    for action in &actions {
        writeln!(log, "- {}", render_action(action)).unwrap();
    }
    writeln!(log, "rows:").unwrap();
    log.push_str(&render_rows(rows));
    writeln!(log, "cursor: {}", engine.cursor()).unwrap();
    writeln!(log).unwrap();
}

/// Add on the server, edit, delete, an expired cursor (410 → full resync)
/// and a local-only row the whole way through: the local-only row is never
/// touched, and every synced row ends where the server says it is.
#[tokio::test]
async fn sync_engine_golden() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = ConversationSqliteRepository::deferred_with_path(dir.path().join("c.db"));

    let api = FakeHiveConversations::new();
    let mut engine = SyncEngine::new(&format!("{SERVER}/"));

    // This device: a local-only conversation, and one it took online before
    // (migrated from `Hosted`): local id ≠ remote id, version 0.
    let mut rows = vec![
        local_only("mine", "Local notes", 50),
        ConversationMetadata {
            sync: ConversationSync::cloud(SERVER, "r-old", None),
            ..local_only("took-online", "Taken online", 40)
        },
    ];
    for row in &rows {
        let mut data = crate::repositories::store_conformance::sample_conversation(
            &row.id,
            &row.title,
            row.updated_at,
        );
        set_sync_columns(&mut data, &row.sync);
        repo.save(&row.id, data).await.unwrap();
    }
    let mine_before = repo.load_one("mine").await.unwrap().unwrap();

    // The server already has the conversation that was taken online.
    api.save("r-old", "Taken online", 40);

    let mut log = String::new();
    step(
        &mut log,
        "first pull (since=0)",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    api.save("r-new", "Started on the web", 100);
    api.save("r-web2", "Another web chat", 110);
    api.save("r-web3", "A third", 120);
    step(
        &mut log,
        "added on the server (paged)",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    api.save("r-new", "Renamed on the web", 130);
    api.save("r-old", "Taken online, continued", 140);
    step(
        &mut log,
        "edited on the server",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    api.delete("r-web2");
    step(
        &mut log,
        "deleted on the server",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    step(
        &mut log,
        "nothing changed",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    // Away long enough for the tombstones to be pruned: the cursor expires,
    // and the deletion is only noticed because the full listing lacks it.
    api.delete("r-web3");
    api.save("r-late", "Created while away", 150);
    api.prune_tombstones();
    step(
        &mut log,
        "cursor expired (410 → full resync)",
        &api,
        &mut engine,
        &mut rows,
        &repo,
    )
    .await;

    assert_eq!(
        repo.load_one("mine").await.unwrap().unwrap().updated_at,
        mine_before.updated_at
    );
    assert_eq!(
        serde_json::to_value(repo.load_one("mine").await.unwrap().unwrap()).unwrap(),
        serde_json::to_value(&mine_before).unwrap(),
        "the local-only row is never touched"
    );

    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &log).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        log,
        expected,
        "{} changed; if that is intended, rerun with UPDATE_GOLDENS=1",
        path.display()
    );
}

/// A row synced to another server is never matched or deleted by a pull
/// against this one, even by a full resync.
#[tokio::test]
async fn rows_synced_elsewhere_are_left_alone() {
    let api = FakeHiveConversations::new();
    let mut engine = SyncEngine::new(SERVER);
    let rows = vec![ConversationMetadata {
        sync: ConversationSync::cloud("http://localhost:8081", "r-dev", None),
        ..local_only("dev", "Developer demo", 1)
    }];
    let actions = pull(&api, &mut engine, &rows).await.unwrap();
    assert!(actions.is_empty(), "{actions:?}");
}

/// A full sync at app start (`since=0`) applies the tombstones the server
/// still holds, but deletes nothing merely because the listing did not name
/// it: only a resync after a 410 does that.
#[tokio::test]
async fn a_start_full_sync_applies_tombstones_but_deletes_nothing_unseen() {
    let api = FakeHiveConversations::new();
    api.save("r-gone", "Deleted while closed", 1);
    api.delete("r-gone");
    let rows = vec![
        ConversationMetadata {
            sync: ConversationSync::cloud(SERVER, "r-gone", None),
            ..local_only("gone", "Deleted while closed", 1)
        },
        ConversationMetadata {
            sync: ConversationSync::cloud(SERVER, "r-unlisted", None),
            ..local_only("unlisted", "Not in the listing", 2)
        },
    ];
    let mut engine = SyncEngine::new(SERVER);
    let actions = pull(&api, &mut engine, &rows).await.unwrap();
    assert_eq!(
        actions,
        vec![SyncAction::ApplyTombstone {
            local_id: "gone".into()
        }]
    );
}

/// A failed pull applies nothing, so it must not move the cursor past the
/// changes it never delivered.
#[tokio::test]
async fn a_failed_pull_keeps_the_cursor() {
    let api = FakeHiveConversations::new();
    let mut engine = SyncEngine::new(SERVER);
    pull(&api, &mut engine, &[]).await.unwrap();
    let cursor = engine.cursor().to_string();

    // Two expiries in one pull: the second is an error.
    api.save("r-1", "x", 1);
    api.prune_tombstones();
    struct AlwaysGone;
    impl HiveConversationsApi for AlwaysGone {
        fn changes_since(
            &self,
            _: &str,
            _: usize,
        ) -> crate::repositories::conversation_repository::BoxFuture<
            'static,
            Result<ChangesPage, ApiError>,
        > {
            Box::pin(async { Err(ApiError::CursorExpired) })
        }
        fn conversation(
            &self,
            _: &str,
        ) -> crate::repositories::conversation_repository::BoxFuture<
            'static,
            Result<ConversationDetail, ApiError>,
        > {
            Box::pin(async { Err(ApiError::NotFound) })
        }
        fn resume(
            &self,
            _: &str,
        ) -> crate::repositories::conversation_repository::BoxFuture<'static, Result<(), ApiError>>
        {
            Box::pin(async { Err(ApiError::NotFound) })
        }
    }
    assert!(matches!(
        pull(&AlwaysGone, &mut engine, &[]).await,
        Err(ApiError::CursorExpired)
    ));
    assert_eq!(engine.cursor(), cursor);
}

// ── Bringing a conversation back keeps its traces ───────────────────────────

async fn conversation_from(data: ConversationData) -> Conversation {
    use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
    use crate::settings::models::models_store::ModelConfig;
    use crate::settings::models::providers_store::{ProviderConfig, ProviderType};

    let _ = crate::init_repositories();
    let model = ModelConfig::new(
        "m1".to_string(),
        "Test Model".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    );
    Conversation::from_data(
        data,
        &model,
        &ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama),
        AgentBuildContext::from_services(AgentServices::default()),
    )
    .await
    .expect("a network-free conversation builds")
}

fn local_row() -> ConversationData {
    let mut data =
        crate::repositories::store_conformance::sample_conversation("local-1", "Mine", 1_000);
    data.message_history = serde_json::json!([
        {"role": "user", "content": [{"type": "text", "text": "hi"}]},
        {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
    ])
    .to_string();
    data.system_traces = r#"[null,{"items":["local trace"]}]"#.to_string();
    data.attachment_paths = r#"[["/tmp/a.png"],[]]"#.to_string();
    data.message_timestamps = "[10,11]".to_string();
    data.message_feedback = r#"[null,"ThumbsUp"]"#.to_string();
    data
}

fn server_detail(traces: Option<serde_json::Value>) -> ConversationDetail {
    let mut value = serde_json::json!({
        "id": "r-1",
        "title": "Mine, continued",
        "model_id": "platform-qwen3",
        "version": 5,
        "placement": "cloud",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
            {"role": "user", "content": [{"type": "text", "text": "and then?"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "this"}]},
        ],
        "message_timestamps": [null, null, 20, 21],
        "token_usage": {
            "message_usages": [{"input_tokens": 7, "output_tokens": 9}],
            "total_input_tokens": 7, "total_output_tokens": 9, "total_estimated_cost_usd": 0.0
        },
        "created_at": 900,
        "updated_at": 2_000,
    });
    if let Some(traces) = traces {
        value["system_traces"] = traces;
    }
    serde_json::from_value(value).expect("the detail decodes")
}

/// The server's traces, timestamps and usage are adopted (D3), and a server
/// that sends no traces for a message this device already had keeps the
/// device's — nothing is wiped, unlike `import_history`.
#[tokio::test]
async fn bringing_a_conversation_back_keeps_its_traces() {
    // The server sends no traces at all (today's chatty-server): the local
    // ones survive on the messages they belong to.
    let mut conversation = conversation_from(local_row()).await;
    conversation
        .replace_from_remote(server_detail(None).to_conversation_data("local-1"))
        .unwrap();
    let data = conversation.to_conversation_data().unwrap();
    assert_eq!(data.id, "local-1", "the local id stays");
    assert_eq!(data.title, "Mine, continued");
    assert_eq!(
        data.model_id, "model-1",
        "the bound model stays this device's"
    );
    assert_eq!(
        data.system_traces,
        r#"[null,{"items":["local trace"]},null,null]"#
    );
    assert_eq!(data.attachment_paths, r#"[["/tmp/a.png"],[],[],[]]"#);
    assert_eq!(data.message_timestamps, "[10,11,20,21]");
    assert_eq!(data.message_feedback, r#"[null,"ThumbsUp",null,null]"#);
    assert_eq!(data.created_at, 900);
    assert_eq!(conversation.token_usage().total_input_tokens, 7);

    // The server sends its own traces: they win, including for the turns
    // this device never saw.
    let mut conversation = conversation_from(local_row()).await;
    conversation
        .replace_from_remote(
            server_detail(Some(serde_json::json!([
                null,
                {"items": ["server trace"]},
                null,
                {"items": ["cloud turn"]}
            ])))
            .to_conversation_data("local-1"),
        )
        .unwrap();
    let data = conversation.to_conversation_data().unwrap();
    assert_eq!(
        data.system_traces,
        r#"[null,{"items":["server trace"]},null,{"items":["cloud turn"]}]"#
    );
}

/// Metadata is only carried across for the same message at the same index:
/// a server history that diverged does not inherit this device's traces.
#[tokio::test]
async fn metadata_never_lands_on_a_different_message() {
    let mut conversation = conversation_from(local_row()).await;
    let mut detail = server_detail(None);
    detail.messages.swap(0, 1);
    conversation
        .replace_from_remote(detail.to_conversation_data("local-1"))
        .unwrap();
    let data = conversation.to_conversation_data().unwrap();
    assert_eq!(data.system_traces, "[null,null,null,null]");
}
