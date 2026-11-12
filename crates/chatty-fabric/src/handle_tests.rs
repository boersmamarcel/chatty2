use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

/// A clock the test moves by hand, in seconds from the epoch.
fn fake_clock() -> (Clock, Arc<AtomicU64>) {
    let secs = Arc::new(AtomicU64::new(1_000_000));
    let read = secs.clone();
    let clock: Clock =
        Arc::new(move || SystemTime::UNIX_EPOCH + Duration::from_secs(read.load(Ordering::SeqCst)));
    (clock, secs)
}

fn scope(id: &str) -> ConversationScope {
    ConversationScope::new(id)
}

fn key(name: &str) -> HandleKey {
    HandleKey::new(scope("conv-a"), name)
}

fn root_owned() -> HandleOwner {
    HandleOwner::new(RunRoot::Owner, HandleParty::Root, Vec::new())
}

fn saved(name: &str, bytes: usize) -> Handle {
    Handle {
        name: name.to_string(),
        agent: "worker".to_string(),
        conversation: SavedConversation::new(json!(["x".repeat(bytes)])),
        tree: None,
        last_task_base: None,
    }
}

/// Run one task on `key` as `owner` that saves a conversation of about
/// `bytes`, and let go of the handle.
fn task(store: &Arc<HandleStore>, key: &HandleKey, owner: HandleOwner, bytes: usize) {
    let expected = store.status(key);
    let ticket = store.begin(key.clone(), owner, &expected).unwrap();
    assert!(ticket.save(saved(&key.name, bytes)));
}

#[test]
fn a_resumed_conversation_is_the_old_one_then_the_new_turns() {
    let saved = SavedConversation::new(json!([{"role": "user", "content": "one"}]));
    let next = saved.extended(&json!([{"role": "assistant", "content": "two"}]));
    assert_eq!(
        next.data,
        json!([
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": "two"}
        ])
    );
    assert_eq!(next.bytes, serde_json::to_vec(&next.data).unwrap().len());
}

/// Invariant 6 (`handle_lifecycle`), on a fake clock: an idle handle ends
/// at 30 min, a ninth handle ends the longest-idle one, saved conversations
/// over 32 MiB end the longest-idle ones, and an owner's end ends the
/// handles it owns. Each ended name answers `Ended`.
#[test]
fn handle_lifecycle() {
    // The spec's numbers, pinned: 30 min, 8 handles, 32 MiB.
    assert_eq!(HANDLE_IDLE_LIMIT, Duration::from_secs(1800));
    assert_eq!(MAX_HANDLES_PER_CONVERSATION, 8);
    assert_eq!(MAX_SAVED_BYTES_PER_CONVERSATION, 32 * 1024 * 1024);

    // 30 min idle.
    let (clock, secs) = fake_clock();
    let store = Arc::new(HandleStore::with_clock(clock));
    task(&store, &key("idle"), root_owned(), 10);
    secs.fetch_add(1799, Ordering::SeqCst);
    assert!(matches!(store.status(&key("idle")), HandleStatus::Live(_)));
    secs.fetch_add(1, Ordering::SeqCst);
    assert_eq!(store.status(&key("idle")), HandleStatus::Ended);

    // A handle with a call running never idles out.
    let expected = store.status(&key("busy"));
    let running = store.begin(key("busy"), root_owned(), &expected).unwrap();
    assert!(running.save(saved("busy", 10)));
    secs.fetch_add(2 * HANDLE_IDLE_LIMIT.as_secs(), Ordering::SeqCst);
    assert!(matches!(store.status(&key("busy")), HandleStatus::Live(_)));
    drop(running);

    // The ninth handle ends the longest-idle of the eight.
    let (clock, secs) = fake_clock();
    let store = Arc::new(HandleStore::with_clock(clock));
    for n in 0..8 {
        task(&store, &key(&format!("h{n}")), root_owned(), 10);
        secs.fetch_add(1, Ordering::SeqCst);
    }
    // h0 was used again last, so h1 is now the longest idle.
    task(&store, &key("h0"), root_owned(), 10);
    task(&store, &key("ninth"), root_owned(), 10);
    assert_eq!(store.status(&key("h1")), HandleStatus::Ended);
    assert_eq!(store.names(&scope("conv-a")).len(), 8);
    assert!(matches!(store.status(&key("h0")), HandleStatus::Live(_)));
    // Another conversation's handles do not count.
    task(
        &store,
        &HandleKey::new(scope("conv-b"), "h0"),
        root_owned(),
        10,
    );
    assert_eq!(store.names(&scope("conv-a")).len(), 8);

    // Over 32 MiB of saved conversation ends the longest-idle handles.
    let (clock, secs) = fake_clock();
    let store = Arc::new(HandleStore::with_clock(clock));
    let mib = 1024 * 1024;
    task(&store, &key("big-old"), root_owned(), 20 * mib);
    secs.fetch_add(1, Ordering::SeqCst);
    task(&store, &key("small"), root_owned(), mib);
    secs.fetch_add(1, Ordering::SeqCst);
    task(&store, &key("big-new"), root_owned(), 12 * mib); // 33 MiB in all
    assert_eq!(store.status(&key("big-old")), HandleStatus::Ended);
    assert!(matches!(store.status(&key("small")), HandleStatus::Live(_)));
    assert!(matches!(
        store.status(&key("big-new")),
        HandleStatus::Live(_)
    ));

    // The owner's end ends the handle, and the handles it owns after it.
    let store = Arc::new(HandleStore::new());
    let node_owned = HandleOwner::new(RunRoot::Owner, HandleParty::Node("lead-0".into()), vec![]);
    task(&store, &key("by-node"), node_owned, 10);
    let handle_owned = HandleOwner::new(
        RunRoot::Owner,
        HandleParty::Handle("by-node".into()),
        vec![HandleParty::Node("lead-0".into())],
    );
    task(&store, &key("grandchild"), handle_owned, 10);
    task(&store, &key("by-root"), root_owned(), 10);
    store.owner_ended(&scope("conv-a"), &HandleParty::Node("lead-0".into()));
    assert_eq!(store.status(&key("by-node")), HandleStatus::Ended);
    assert_eq!(store.status(&key("grandchild")), HandleStatus::Ended);
    assert!(matches!(
        store.status(&key("by-root")),
        HandleStatus::Live(_)
    ));
    // The root conversation's end ends the rest, and forgets the names it
    // ended: nobody is left in it to call them.
    store.end_conversation(&scope("conv-a"));
    assert_eq!(store.names(&scope("conv-a")), Vec::<String>::new());
    assert_eq!(store.status(&key("by-root")), HandleStatus::Free);
    assert_eq!(store.status(&key("by-node")), HandleStatus::Free);
    // The owner dropping a handle ends it.
    task(&store, &key("dropped"), root_owned(), 10);
    assert!(store.drop_handle(&key("dropped")));
    assert_eq!(store.status(&key("dropped")), HandleStatus::Ended);
}

/// A call queued behind a handle that ends while it waits finds it
/// expired, and a save from a call whose handle ended keeps nothing.
#[test]
fn a_call_whose_handle_ended_resumes_nothing() {
    let store = Arc::new(HandleStore::new());
    let running = store
        .begin(key("h"), root_owned(), &HandleStatus::Free)
        .unwrap();
    let live = store.status(&key("h"));
    let queued = store.begin(key("h"), root_owned(), &live).unwrap();
    store.end_conversation(&scope("conv-a"));
    assert!(!running.save(saved("h", 10)));
    assert_eq!(
        queued.saved(),
        Err(HandleError::Expired {
            name: "h".to_string()
        })
    );
}

/// The gate's read is checked again when the call joins: a name another
/// call took in between is not joined on a stale read.
#[test]
fn begin_refuses_a_stale_read() {
    let store = Arc::new(HandleStore::new());
    let _first = store
        .begin(key("h"), root_owned(), &HandleStatus::Free)
        .unwrap();
    assert!(
        store
            .begin(key("h"), root_owned(), &HandleStatus::Free)
            .is_err()
    );
}

/// A first task that captured nothing leaves the name free again.
#[test]
fn a_handle_with_nothing_saved_goes_with_its_last_call() {
    let store = Arc::new(HandleStore::new());
    let ticket = store
        .begin(key("h"), root_owned(), &HandleStatus::Free)
        .unwrap();
    ticket.clear();
    drop(ticket);
    assert_eq!(store.status(&key("h")), HandleStatus::Free);
}

#[test]
fn the_owner_and_its_ancestors_are_admitted_and_no_one_else() {
    let owner = HandleOwner::new(
        RunRoot::Owner,
        HandleParty::Node("coder-1".into()),
        vec![HandleParty::Handle("lead".into())],
    );
    assert_eq!(owner.line.last(), Some(&HandleParty::Root));
    for party in [
        HandleParty::Node("coder-1".into()),
        HandleParty::Handle("lead".into()),
        HandleParty::Root,
    ] {
        assert!(owner.admits(&RunRoot::Owner, &party), "{party}");
    }
    assert!(!owner.admits(&RunRoot::Owner, &HandleParty::Node("coder-2".into())));
    // The same party under another root kind is someone else.
    let key = RunRoot::External { key: "k1".into() };
    assert!(!owner.admits(&key, &HandleParty::Root));
}

#[test]
fn handle_errors_are_typed_on_the_wire() {
    let expired = HandleError::Expired { name: "fix".into() };
    assert_eq!(
        serde_json::to_value(&expired).unwrap(),
        json!({"code": "handle_expired", "name": "fix"})
    );
    assert!(expired.to_string().starts_with("handle_expired: "));
    let not_owner = HandleError::NotOwner {
        name: "fix".into(),
        caller: "node 'x-0'".into(),
        owner: "root".into(),
    };
    assert_eq!(
        serde_json::to_value(&not_owner).unwrap()["code"],
        "not_owner"
    );
}

/// Calls to one handle run one at a time, in the order they arrived.
#[tokio::test(start_paused = true)]
async fn a_handle_s_calls_queue_in_arrival_order() {
    let store = Arc::new(HandleStore::new());
    let order = Arc::new(Mutex::new(Vec::new()));
    let first = store
        .begin(key("h"), root_owned(), &HandleStatus::Free)
        .unwrap();
    let held = first.queue().lock_owned().await;
    let mut waiting = Vec::new();
    for n in 0..3 {
        let live = store.status(&key("h"));
        let ticket = store.begin(key("h"), root_owned(), &live).unwrap();
        let order = order.clone();
        waiting.push(tokio::spawn(async move {
            let _held = ticket.queue().lock_owned().await;
            order.lock().unwrap().push(n);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }));
        // Each one queued before the next arrives.
        tokio::task::yield_now().await;
    }
    assert!(order.lock().unwrap().is_empty(), "the first call holds it");
    drop(held);
    for task in waiting {
        task.await.unwrap();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
}

/// A worker that is reaped no longer calls as its handle.
#[test]
fn a_reaped_worker_no_longer_serves_its_handle() {
    let store = Arc::new(HandleStore::new());
    let ticket = store
        .begin(key("h"), root_owned(), &HandleStatus::Free)
        .unwrap();
    ticket.served_by("worker-0");
    assert_eq!(store.served_by("worker-0"), Some(key("h")));
    assert!(ticket.save(saved("h", 10)));
    drop(ticket);
    assert_eq!(store.served_by("worker-0"), None);
}
