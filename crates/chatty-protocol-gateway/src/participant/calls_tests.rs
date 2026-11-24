use super::*;
use chatty_fabric::{PENDING_LIST_BYTES, Remaining, SENDER_ALLOWANCE_BYTES};
use serde_json::json;

fn broker(edges: Option<Arc<Mutex<EdgeLog>>>) -> (Arc<BrokerCalls>, ParticipantRegistry) {
    let registry = ParticipantRegistry::new();
    let calls = Arc::new(BrokerCalls::new(
        registry.clone(),
        Arc::new(BTreeMap::new()),
        edges,
    ));
    registry.install_calls(&calls);
    (calls, registry)
}

/// The edge log's rows as `(kind, from, to, bytes, outcome)`.
fn rows(path: &std::path::Path) -> Vec<(EdgeKind, String, String, u64, String)> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<EdgeRow>(line).unwrap())
        .map(|row| (row.kind, row.from, row.to, row.bytes, row.outcome))
        .collect()
}

async fn send(calls: &BrokerCalls, caller: Peer, to: &str, text: &str) -> MessageStatus {
    let mut stream = calls.call(
        caller,
        CallRequest::SendMessage(SendMessageParams {
            to: to.to_string(),
            text: text.to_string(),
            run: None,
        }),
    );
    let Some(Ok(CallEvent::Result(CallResult::Posted(status)))) = stream.next().await else {
        panic!("a send_message call answers with one status");
    };
    assert!(stream.next().await.is_none());
    status
}

fn node(name: &str) -> Peer {
    Peer::Node(name.to_string())
}

fn refused(reason: RefusalReason) -> MessageStatus {
    MessageStatus::Refused { reason }
}

/// Up the tree, the owner is the only recipient: the root for a node
/// the root owns, the owning node for one a node owns; siblings, self,
/// a grandparent, unknown names, an unknown sender and the root itself
/// are refused. (Down the tree is TM-5's, below.)
#[tokio::test]
async fn only_the_owner_is_on_the_tree() {
    let (calls, registry) = broker(None);
    let lead = registry.admit_running("lead", None);
    let coder = registry.admit_running("coder", Some(&lead));
    let other = registry.admit_running("coder", Some(&lead));

    assert_eq!(
        send(&calls, node(&lead), ROOT_NAME, "up").await,
        MessageStatus::Pending { id: "msg-1".into() }
    );
    assert_eq!(
        send(&calls, node(&coder), &lead, "up").await,
        MessageStatus::Pending { id: "msg-2".into() }
    );
    for (from, to) in [
        (coder.as_str(), other.as_str()),
        (coder.as_str(), coder.as_str()),
        (coder.as_str(), ROOT_NAME),
        (coder.as_str(), "nobody-0"),
        ("never-admitted-0", ROOT_NAME),
    ] {
        assert_eq!(
            send(&calls, node(from), to, "x").await,
            refused(RefusalReason::NotOnTree),
            "{from} -> {to}"
        );
    }
    assert_eq!(
        send(&calls, Peer::Root, "nobody-0", "x").await,
        refused(RefusalReason::NotOnTree),
        "the human reaches nodes, not names nobody has"
    );
}

async fn take(calls: &BrokerCalls, caller: Peer) -> Result<Vec<String>, CallError> {
    let mut stream = calls.call(caller, CallRequest::TakeMessages);
    match stream.next().await {
        Some(Ok(CallEvent::Result(CallResult::Messages(messages)))) => Ok(messages),
        Some(Err(error)) => Err(error),
        other => panic!("a mailbox.take call answers with its messages: {other:?}"),
    }
}

/// TM-5: a node's message to its own running child goes on the child's
/// mid-run list, which the child's next tool round takes — wrapped,
/// once, with a `delivered_mid_run` row — and never on the list TM-2's
/// invoke result delivers. The human reaches any node the same way.
#[tokio::test]
async fn owner_message_reaches_own_child_mid_run() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
    let lead = registry.admit_running("lead", None);
    let coder = registry.admit_running("coder", Some(&lead));

    assert!(matches!(
        send(&calls, node(&lead), &coder, "stop that, look at X").await,
        MessageStatus::Pending { .. }
    ));
    assert!(matches!(
        send(&calls, Peer::Root, &coder, "and Y").await,
        MessageStatus::Pending { .. }
    ));
    assert_eq!(
        take(&calls, node(&coder)).await.unwrap(),
        [
            format!("<message from=\"{lead}\" untrusted=\"true\">stop that, look at X</message>"),
            format!("<message from=\"{ROOT_NAME}\" untrusted=\"true\">and Y</message>"),
        ]
    );
    assert!(
        take(&calls, node(&coder)).await.unwrap().is_empty(),
        "taken once"
    );
    assert!(
        calls.start_run(&node(&coder)).is_empty(),
        "nothing is left for the child's next run"
    );
    let delivered: Vec<_> = rows(&path)
        .into_iter()
        .filter(|row| row.4 == "delivered_mid_run")
        .collect();
    assert_eq!(
        delivered,
        [
            (
                EdgeKind::Message,
                lead.clone(),
                coder.clone(),
                20,
                "delivered_mid_run".into()
            ),
            (
                EdgeKind::Message,
                ROOT_NAME.to_string(),
                coder.clone(),
                5,
                "delivered_mid_run".into()
            ),
        ]
    );

    // A message no tool round took opens the child's next run.
    send(&calls, node(&lead), &coder, "late").await;
    assert_eq!(
        calls.start_run(&node(&coder)),
        [format!(
            "<message from=\"{lead}\" untrusted=\"true\">late</message>"
        )]
    );
    // The root takes nothing mid-run: its messages open its next run.
    assert!(matches!(
        take(&calls, Peer::Root).await,
        Err(CallError::Refused(_))
    ));
}

/// TM-5 leaves sideways messages gated (F3): a node's message to its
/// sibling — mid-run or not — is `not_on_tree`, and the sibling's
/// tool rounds and next run find nothing.
#[tokio::test]
async fn sibling_mid_run_message_still_refused() {
    let (calls, registry) = broker(None);
    let lead = registry.admit_running("lead", None);
    let coder = registry.admit_running("coder", Some(&lead));
    let other = registry.admit_running("coder", Some(&lead));
    let grandchild = registry.admit_running("coder", Some(&coder));

    for (from, to) in [
        (&coder, &other),
        (&lead, &grandchild),
        (&grandchild, &other),
    ] {
        assert_eq!(
            send(&calls, node(from), to, "do it my way").await,
            refused(RefusalReason::NotOnTree),
            "{from} -> {to}"
        );
    }
    for recipient in [&other, &grandchild] {
        assert!(take(&calls, node(recipient)).await.unwrap().is_empty());
        assert!(calls.start_run(&node(recipient)).is_empty());
    }
}

#[tokio::test]
async fn a_message_to_an_ended_owner_is_refused() {
    let (calls, registry) = broker(None);
    let lead = registry.admit_running("lead", None);
    let coder = registry.admit_running("coder", Some(&lead));
    registry.end_node(&lead);
    assert_eq!(
        send(&calls, node(&coder), &lead, "too late").await,
        refused(RefusalReason::RecipientEnded)
    );
}

/// The pending list's bounds hold through the broker: per sender per
/// run, then per recipient, each recipient with a list of its own.
#[tokio::test]
async fn the_broker_enforces_the_pending_list_bounds() {
    let (calls, registry) = broker(None);
    let lead = registry.admit_running("lead", None);
    let coders: Vec<String> = (0..9)
        .map(|_| registry.admit_running("coder", Some(&lead)))
        .collect();
    let allowance = "x".repeat(SENDER_ALLOWANCE_BYTES);

    assert!(matches!(
        send(&calls, node(&coders[0]), &lead, &allowance).await,
        MessageStatus::Pending { .. }
    ));
    assert_eq!(
        send(&calls, node(&coders[0]), &lead, "one byte more").await,
        refused(RefusalReason::OverAllowance)
    );
    for coder in &coders[1..PENDING_LIST_BYTES / SENDER_ALLOWANCE_BYTES] {
        assert!(matches!(
            send(&calls, node(coder), &lead, &allowance).await,
            MessageStatus::Pending { .. }
        ));
    }
    assert_eq!(
        send(&calls, node(&coders[8]), &lead, "x").await,
        refused(RefusalReason::OverAllowance),
        "the lead's list is full"
    );
    assert!(
        matches!(
            send(&calls, node(&lead), ROOT_NAME, &allowance).await,
            MessageStatus::Pending { .. }
        ),
        "the root's list is another list"
    );
}

/// Rule 5: a recipient that ends drops what was waiting for it, one
/// `dropped` row per message; nothing delivers them afterwards, and a
/// later message to it is `recipient_ended`. Another recipient's list
/// is untouched.
#[tokio::test]
async fn messages_dropped_with_recipient() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
    let lead = registry.admit_running("lead", None);
    let coder = registry.admit_running("coder", Some(&lead));
    let other = registry.admit_running("coder", Some(&lead));

    send(&calls, node(&coder), &lead, "first").await;
    send(&calls, node(&other), &lead, "second one").await;
    send(&calls, node(&lead), ROOT_NAME, "for the root").await;
    registry.end_node(&lead);

    assert!(
        calls.start_run(&node(&lead)).is_empty(),
        "nothing is left to deliver"
    );
    assert_eq!(
        send(&calls, node(&coder), &lead, "too late").await,
        refused(RefusalReason::RecipientEnded)
    );
    let dropped: Vec<_> = rows(&path)
        .into_iter()
        .filter(|row| row.4 == "dropped")
        .collect();
    assert_eq!(
        dropped,
        [
            (
                EdgeKind::Message,
                coder.clone(),
                lead.clone(),
                5,
                "dropped".into()
            ),
            (
                EdgeKind::Message,
                other.clone(),
                lead.clone(),
                10,
                "dropped".into()
            ),
        ]
    );
    assert_eq!(
        calls.start_run(&Peer::Root),
        [format!(
            "<message from=\"{lead}\" untrusted=\"true\">for the root</message>"
        )],
        "the root's own list is not the ended node's"
    );
}

/// TM-4: messages waiting for a handle that goes, and messages taken
/// for a task whose worker never starts, are not lost silently: each
/// leaves a `dropped` row.
#[tokio::test]
async fn handle_messages_that_go_nowhere_are_dropped_with_a_row() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let edges = Some(Arc::new(Mutex::new(log)));
    let (calls, _registry) = broker(edges.clone());
    let message = |text: &str| Message {
        id: "msg-1".to_string(),
        from: Sender::Root,
        from_name: "lead-0".to_string(),
        text: text.to_string(),
    };

    // The conversation ends with a message waiting for its handle.
    let key = HandleKey::new(ConversationScope::new("conv-a"), "fixer");
    locked(&calls.handle_pending)
        .entry(key.clone())
        .or_default()
        .push(message("hello"))
        .unwrap();
    calls.end_conversation(&key.scope);
    assert!(locked(&calls.handle_pending).is_empty());

    // The call taking messages for a task ends before its worker starts.
    drop(Undelivered {
        edges,
        to: "fixer".to_string(),
        messages: vec![message("again")],
    });
    assert_eq!(
        rows(&path),
        [
            (
                EdgeKind::Message,
                "lead-0".to_string(),
                "fixer".to_string(),
                5,
                "dropped".to_string()
            ),
            (
                EdgeKind::Message,
                "lead-0".to_string(),
                "fixer".to_string(),
                5,
                "dropped".to_string()
            ),
        ]
    );
}

/// AGE-1346: a clear that races the check and the push (the task
/// captured no conversation, cleared the mail and freed the name) must
/// not leave the message on a freed key for a later handle of that
/// name. The hook frees the handle in place and starts the mail clear
/// on another thread, which needs the pending lock: held across check
/// and push, it waits until the push is in and drops the message with
/// one `dropped` row. Without the lock the clear runs first and the
/// message stays on the freed key.
#[tokio::test]
async fn handle_mail_is_not_left_on_a_key_freed_between_check_and_push() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
    let lead = registry.admit_running("lead", None);
    let key = HandleKey::new(ConversationScope::new(format!("node:{lead}")), "fixer");
    let owner = HandleOwner::new(RunRoot::Owner, HandleParty::Node(lead.clone()), Vec::new());
    let ticket = calls
        .handles
        .begin(key.clone(), owner, &HandleStatus::Free)
        .unwrap();
    assert!(ticket.save(Handle {
        name: "fixer".to_string(),
        agent: "worker".to_string(),
        conversation: SavedConversation::new(json!([])),
        tree: None,
        last_task_base: None,
    }));
    drop(ticket);
    let clear = Arc::new(Mutex::new(None));
    let hook_calls = calls.clone();
    let hook_key = key.clone();
    let hook_clear = clear.clone();
    *locked(&calls.before_handle_push) = Some(Box::new(move || {
        hook_calls.handles.drop_handle(&hook_key);
        let (calls, key) = (hook_calls.clone(), hook_key.clone());
        *locked(&hook_clear) = Some(std::thread::spawn(move || {
            drop_handle_mail(&calls.handle_pending, &calls.edges, &key);
        }));
        // Time for an unguarded clear to finish first.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }));

    let status = send(&calls, node(&lead), "fixer", "hello").await;

    locked(&clear).take().unwrap().join().unwrap();
    assert_eq!(status, MessageStatus::Pending { id: "msg-1".into() });
    assert!(
        locked(&calls.handle_pending).is_empty(),
        "nothing waits on the freed key"
    );
    let dropped = rows(&path)
        .into_iter()
        .filter(|row| row.4 == "dropped")
        .count();
    assert_eq!(dropped, 1);
}

/// A node `lead` with one open run under each of two root
/// conversations, `conv-a` and `conv-b`, and an idle handle `fixer`
/// that `lead` owns in both. Returns the broker, `lead`, the two runs'
/// names and the two handles' keys, as `(a, b)` pairs.
#[allow(clippy::type_complexity)]
fn lead_in_two_conversations() -> (
    Arc<BrokerCalls>,
    ParticipantRegistry,
    String,
    (String, String),
    (HandleKey, HandleKey),
) {
    let (calls, registry) = broker(None);
    let lead = registry.admit_under("lead", None);
    let mut runs = Vec::new();
    let mut keys = Vec::new();
    for (root, conversation) in [("t-a", "conv-a"), ("t-b", "conv-b")] {
        let chain = CallChain::root(root).extend("lead").unwrap();
        std::mem::forget(registry.open_run(&lead, None, None, chain).unwrap());
        locked(&calls.scopes).insert(root.to_string(), ConversationScope::new(conversation));
        let (_, open) = registry.open_runs_of(&lead).unwrap();
        runs.push(
            open.into_iter()
                .find(|run| run.chain.root_task_id == root)
                .unwrap()
                .name,
        );
        let key = HandleKey::new(ConversationScope::new(conversation), "fixer");
        let owner = HandleOwner::new(RunRoot::Owner, HandleParty::Node(lead.clone()), Vec::new());
        let ticket = calls
            .handles
            .begin(key.clone(), owner, &HandleStatus::Free)
            .unwrap();
        assert!(ticket.save(Handle {
            name: "fixer".to_string(),
            agent: "worker".to_string(),
            conversation: SavedConversation::new(json!([])),
            tree: None,
            last_task_base: None,
        }));
        drop(ticket);
        keys.push(key);
    }
    let (b, a) = (runs.pop().unwrap(), runs.pop().unwrap());
    let (key_b, key_a) = (keys.pop().unwrap(), keys.pop().unwrap());
    (calls, registry, lead, (a, b), (key_a, key_b))
}

async fn send_from_run(
    calls: &BrokerCalls,
    lead: &str,
    run: Option<&str>,
    to: &str,
) -> MessageStatus {
    let mut stream = calls.call(
        node(lead),
        CallRequest::SendMessage(SendMessageParams {
            to: to.to_string(),
            text: "hello".to_string(),
            run: run.map(str::to_string),
        }),
    );
    let Some(Ok(CallEvent::Result(CallResult::Posted(status)))) = stream.next().await else {
        panic!("a send_message call answers with one status");
    };
    status
}

fn waiting(calls: &BrokerCalls, key: &HandleKey) -> usize {
    locked(&calls.handle_pending)
        .get(key)
        .map_or(0, |list| list.len())
}

/// AGE-1349: a node with runs under two conversations that each hold a
/// handle of the same name reaches, from each run, only that run's
/// conversation's handle, whichever scope the node's list names first.
#[tokio::test]
async fn a_node_in_two_conversations_messages_only_its_runs_handle() {
    let (calls, _registry, lead, (run_a, run_b), (key_a, key_b)) = lead_in_two_conversations();

    let status = send_from_run(&calls, &lead, Some(&run_a), "fixer").await;
    assert_eq!(status, MessageStatus::Pending { id: "msg-1".into() });
    assert_eq!((waiting(&calls, &key_a), waiting(&calls, &key_b)), (1, 0));

    let status = send_from_run(&calls, &lead, Some(&run_b), "fixer").await;
    assert_eq!(status, MessageStatus::Pending { id: "msg-2".into() });
    assert_eq!((waiting(&calls, &key_a), waiting(&calls, &key_b)), (1, 1));

    // A run that is not the node's own is refused, not trusted.
    let status = send_from_run(&calls, &lead, Some("not-my-run"), "fixer").await;
    assert_eq!(status, refused(RefusalReason::NotOnTree));
    assert_eq!((waiting(&calls, &key_a), waiting(&calls, &key_b)), (1, 1));
}

/// AGE-1349: an older worker names no run; its post is read under the
/// node's whole scope, as before, and reaches a handle of its node.
#[tokio::test]
async fn a_post_without_run_falls_back_to_the_node_scope() {
    let (calls, _registry, lead, _runs, (key_a, key_b)) = lead_in_two_conversations();

    let status = send_from_run(&calls, &lead, None, "fixer").await;
    assert_eq!(status, MessageStatus::Pending { id: "msg-1".into() });
    assert_eq!(waiting(&calls, &key_a) + waiting(&calls, &key_b), 1);
}

/// RO-4: a handle's roster row counts the messages waiting for it, in
/// its own conversation only, and says who owns it and whether a call
/// holds it.
#[tokio::test]
async fn handle_rows_count_waiting_mail() {
    let (calls, _registry) = broker(None);
    let scope = ConversationScope::new("conv-a");
    let key = HandleKey::new(scope.clone(), "fixer");
    let owner = HandleOwner::new(RunRoot::Owner, HandleParty::Root, Vec::new());
    let ticket = calls
        .handles
        .begin(key.clone(), owner, &HandleStatus::Free)
        .unwrap();
    assert!(ticket.save(Handle {
        name: "fixer".to_string(),
        agent: "worker".to_string(),
        conversation: SavedConversation::new(json!([])),
        tree: None,
        last_task_base: None,
    }));
    drop(ticket);
    locked(&calls.handle_pending)
        .entry(key)
        .or_default()
        .push(Message {
            id: "msg-1".to_string(),
            from: Sender::Root,
            from_name: "lead-0".to_string(),
            text: "hello".to_string(),
        })
        .unwrap();

    let rows = calls.handle_rows(&scope);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (
            rows[0].name.as_str(),
            rows[0].owner.as_str(),
            rows[0].busy,
            rows[0].pending_messages
        ),
        ("fixer", ROOT_NAME, false, 1)
    );
    assert!(
        calls
            .handle_rows(&ConversationScope::new("conv-b"))
            .is_empty(),
        "another conversation sees none"
    );
}

/// Delivery point b for the root: its next run takes what is waiting,
/// wrapped and oldest first, exactly once, and gives each sender its
/// allowance back.
#[tokio::test]
async fn the_roots_next_run_takes_its_messages() {
    let (calls, registry) = broker(None);
    let lead = registry.admit_running("lead", None);
    let root = DirectTransport::new(calls.clone());
    let allowance = "x".repeat(SENDER_ALLOWANCE_BYTES - 3);

    send(&calls, node(&lead), ROOT_NAME, "one").await;
    send(&calls, node(&lead), ROOT_NAME, &allowance).await;
    assert_eq!(
        send(&calls, node(&lead), ROOT_NAME, "x").await,
        refused(RefusalReason::OverAllowance)
    );
    let wrap = |text: &str| format!("<message from=\"{lead}\" untrusted=\"true\">{text}</message>");
    assert_eq!(root.take_run_messages(), [wrap("one"), wrap(&allowance)]);
    assert!(root.take_run_messages().is_empty(), "delivered once");
    assert!(
        matches!(
            send(&calls, node(&lead), ROOT_NAME, "x").await,
            MessageStatus::Pending { .. }
        ),
        "a new run gives the sender its allowance back"
    );
}

/// Delivery point b for a node: its next task opens with what is
/// waiting for it.
#[tokio::test]
async fn a_nodes_next_task_opens_with_its_messages() {
    use super::super::protocol::{BrokerFrame, ParticipantCard};

    let (calls, registry) = broker(None);
    let admitted = registry.admit("lead", AgentOrigin::Local, None).unwrap();
    let (tx, mut outbound) = tokio::sync::mpsc::channel(8);
    let lead = registry.register(admitted, ParticipantCard::default(), tx);
    let coder = registry.admit_running("coder", Some(&lead));
    send(&calls, node(&coder), &lead, "<b>tests pass</b>").await;

    registry
        .submit_task(&lead, DelegatedTask::from_root("next task"))
        .await
        .expect("the lead is registered");
    let Some(BrokerFrame::Task { text, .. }) = outbound.recv().await else {
        panic!("a task frame");
    };
    assert_eq!(
        text,
        format!(
            "<message from=\"{coder}\" untrusted=\"true\">&lt;b&gt;tests pass&lt;/b&gt;</message>\n\nnext task"
        )
    );
    registry
        .submit_task(&lead, DelegatedTask::from_root("and another"))
        .await
        .unwrap();
    let Some(BrokerFrame::Task { text, .. }) = outbound.recv().await else {
        panic!("a task frame");
    };
    assert_eq!(text, "and another", "delivered once");
}

/// One message row per call, pending or refused, with the body's size.
#[tokio::test]
async fn every_message_writes_one_message_row() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let (calls, registry) = broker(Some(Arc::new(Mutex::new(log))));
    let lead = registry.admit_running("lead", None);

    send(&calls, node(&lead), ROOT_NAME, "hello").await;
    send(&calls, node(&lead), "nobody-0", "hi").await;

    let rows: Vec<EdgeRow> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rows: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.kind,
                row.from.as_str(),
                row.to.as_str(),
                row.bytes,
                row.outcome.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (EdgeKind::Message, lead.as_str(), ROOT_NAME, 5, "pending"),
            (
                EdgeKind::Message,
                lead.as_str(),
                "nobody-0",
                2,
                "refused: not_on_tree"
            ),
        ]
    );
}

/// A virtual agent that counts the workers it was asked for and starts
/// none.
struct Counting {
    name: String,
    registry: ParticipantRegistry,
    spawned: Arc<AtomicU64>,
}

impl VirtualAgent for Counting {
    fn agent_name(&self) -> &str {
        &self.name
    }

    fn agent_card(&self) -> super::super::protocol::ParticipantCard {
        super::super::protocol::ParticipantCard {
            name: self.name.clone(),
            display_name: None,
            description: String::new(),
            version: String::new(),
            skills: Vec::new(),
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, _task: DelegatedTask) -> super::super::virtual_agent::WorkerFuture<'_> {
        self.spawned.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(anyhow::anyhow!("nothing is spawned here")) })
    }
}

/// A virtual agent whose one worker takes `delay` to answer: what a
/// fake model that `Delay`s looks like from the broker. It records the
/// budget its task frame would carry and when its worker was reaped.
struct Slow {
    registry: ParticipantRegistry,
    delay: std::time::Duration,
    budget: Arc<Mutex<Option<Remaining>>>,
    reaped: Arc<Mutex<Option<tokio::time::Instant>>>,
}

struct SlowWorker {
    reaped: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl super::super::virtual_agent::WorkerHandle for SlowWorker {
    fn name(&self) -> &str {
        "kit-slow-0"
    }

    fn task_id(&self) -> Option<&str> {
        Some("task-slow")
    }

    fn finish(&mut self, _succeeded: bool, _metadata: Option<&chatty_fabric::wire::TaskMetadata>) {}
}

impl Drop for SlowWorker {
    fn drop(&mut self) {
        *self.reaped.lock().unwrap() = Some(tokio::time::Instant::now());
    }
}

impl VirtualAgent for Slow {
    fn agent_name(&self) -> &str {
        "kit-slow"
    }

    fn agent_card(&self) -> super::super::protocol::ParticipantCard {
        super::super::protocol::ParticipantCard {
            name: "kit-slow".to_string(),
            display_name: None,
            description: String::new(),
            version: String::new(),
            skills: Vec::new(),
        }
    }

    fn registry(&self) -> &ParticipantRegistry {
        &self.registry
    }

    fn run_task(&self, task: DelegatedTask) -> super::super::virtual_agent::WorkerFuture<'_> {
        *self.budget.lock().unwrap() = Some(task.frame_budget());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let delay = self.delay;
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(TaskUpdate::Status {
                state: TaskState::Completed,
                message: None,
                metadata: None,
            });
        });
        let worker = SlowWorker {
            reaped: self.reaped.clone(),
        };
        Box::pin(async move {
            Ok((
                Box::new(worker) as Box<dyn super::super::virtual_agent::WorkerHandle>,
                rx,
            ))
        })
    }
}

/// Invariant 5 (DP-3): a callee started with 30 s left whose model
/// takes 40 s is stopped by its deadline (plus the grace a headless run
/// gives its own pass), and the caller gets a failed result, not a
/// hang. On tokio's paused clock: no real second passes.
#[tokio::test(start_paused = true)]
async fn deadline_propagates() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let registry = ParticipantRegistry::new();
    let budget = Arc::new(Mutex::new(None));
    let reaped = Arc::new(Mutex::new(None));
    let slow: Arc<dyn VirtualAgent> = Arc::new(Slow {
        registry: registry.clone(),
        delay: std::time::Duration::from_secs(40),
        budget: budget.clone(),
        reaped: reaped.clone(),
    });
    let calls = BrokerCalls::new(
        registry.clone(),
        Arc::new(BTreeMap::from([("kit-slow".to_string(), slow)])),
        Some(Arc::new(Mutex::new(log))),
    );

    let start = tokio::time::Instant::now();
    let events: Vec<_> = calls
        .call(
            Peer::Root,
            CallRequest::InvokeAgent(InvokeAgentParams {
                agent: "kit-slow".to_string(),
                prompt: "Take your time.".to_string(),
                handle: None,
                include_trace: false,
                files: None,
                spawn_context: None,
                remaining: Remaining {
                    seconds: Some(30),
                    ..Remaining::default()
                },
                run: None,
            }),
        )
        .collect()
        .await;
    let ended = start.elapsed();

    let Some(Ok(CallEvent::Result(CallResult::Invoked(outcome)))) = events.last() else {
        panic!("the call ends with a result: {events:?}");
    };
    assert!(!outcome.success, "{outcome:?}");
    assert!(
        outcome
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("deadline:")),
        "{outcome:?}"
    );

    let seconds = budget.lock().unwrap().clone().unwrap().seconds;
    assert!(
        seconds.is_some_and(|s| (29..=30).contains(&s)),
        "the callee's task carries the 30 s its caller had left: {seconds:?}"
    );
    let grace = deadline_grace(std::time::Duration::from_secs(30));
    assert!(
        ended <= std::time::Duration::from_secs(30) + grace,
        "stopped by the deadline, after {ended:?}"
    );
    assert!(
        ended < std::time::Duration::from_secs(40),
        "not after the model answered"
    );
    let reaped = reaped.lock().unwrap().expect("the worker was reaped");
    assert!(
        reaped - start <= ended,
        "the worker was reaped with the call"
    );

    assert_eq!(
        rows(&path),
        [(
            EdgeKind::Task,
            ROOT_NAME.to_string(),
            "kit-slow-0".to_string(),
            "Take your time.".len() as u64,
            "failed".to_string()
        )]
    );
}

/// What the gate decides for an `agent.invoke` of the registered spec
/// `agent` over `calls`'s policy: from the root when `node` is `None`,
/// else from the node `node`, calling from its one run at `chain`.
fn decide_invoke(
    calls: &BrokerCalls,
    node: Option<&str>,
    chain: CallChain,
    agent: &str,
) -> Result<CallStamp, Refused> {
    let caller = match node {
        None => Caller::Root,
        Some(name) => Caller::Node(NodeCaller {
            name: name.to_string(),
            spec: chain.chain.last().cloned().expect("a chain"),
            runs: vec![super::super::gate::OpenRun {
                name: "task-1".to_string(),
                id: serde_json::from_value(json!(1)).expect("a run id"),
                chain: chain.clone(),
            }],
        }),
    };
    let params = InvokeAgentParams {
        agent: agent.to_string(),
        prompt: "go".to_string(),
        handle: None,
        include_trace: false,
        files: None,
        spawn_context: None,
        remaining: Remaining::default(),
        run: node.map(|_| "task-1".to_string()),
    };
    let snapshot = Ok(Snapshot {
        policy: calls.policy.get(),
        now: std::time::SystemTime::now(),
        root_task_id: chain.root_task_id.clone(),
        callee: Callee::Node {
            name: format!("{agent}-0"),
            spec: agent.to_string(),
        },
        owner: Owner::None,
        addressee: Addressee::None,
        binding: None,
        answer_nonce: None,
        handle: HandleCheck::None,
    });
    match gate::decide(&caller, &Request::Invoke(&params), &snapshot).outcome {
        Ok(Grant::Invoke { stamp, .. }) => Ok(stamp),
        Ok(other) => panic!("agent.invoke granted as {other:?}"),
        Err(refused) => Err(refused),
    }
}

/// EN-0a (AGE-765), ported to the gate (GT-0): a broker built with no
/// policy consults the named [`LocalPermissive`], and delegation
/// behaves as it did when the check was skipped: the root and every
/// node may call any spec, no spec brings a budget, and the chain still
/// refuses a cycle or a call too deep.
#[test]
fn absent_policy_is_local_permissive() {
    let (calls, registry) = broker(None);
    assert!(
        matches!(calls.policy, Policy::LocalPermissive(_)),
        "no policy is LocalPermissive"
    );
    let given_none = BrokerCalls::new(registry, Arc::new(BTreeMap::new()), None).with_policy(None);
    assert!(
        matches!(given_none.policy, Policy::LocalPermissive(_)),
        "`with_policy(None)` is LocalPermissive too"
    );

    let root = CallChain::root("t-permissive");
    let stamp = decide_invoke(&calls, None, root.clone(), "anyone")
        .expect("a plain root may call any spec");
    assert_eq!(stamp.caller, None);
    assert_eq!(stamp.chain.chain, ["root", "anyone"]);
    assert_eq!(stamp.chain.remaining, Remaining::default(), "no own budget");

    let at_one = root.extend("kit-1").unwrap();
    let stamp = decide_invoke(&calls, Some("kit-1-0"), at_one.clone(), "kit-2")
        .expect("a node may call any spec");
    assert_eq!(stamp.caller.as_deref(), Some("kit-1-0"));
    assert_eq!(stamp.chain.chain, ["root", "kit-1", "kit-2"]);

    assert!(matches!(
        decide_invoke(&calls, Some("kit-1-0"), at_one, "kit-1"),
        Err(Refused::Delegation(Refusal::Cycle { .. }))
    ));
    let mut deepest = CallChain::root("t-deep");
    for spec in ["a", "b", "c", "d"] {
        deepest = deepest.extend(spec).unwrap();
    }
    assert_eq!(
        decide_invoke(&calls, Some("d-0"), deepest, "e").err(),
        Some(Refused::Delegation(Refusal::TooDeep { depth: 5, max: 4 }))
    );
}

/// Invariant 4 (DP-2): a worker at depth 4 calls on. The broker reads
/// the caller's chain from its own task table, never from the frame,
/// and refuses the call at its real depth — and a cycle as a cycle —
/// with nothing spawned. (A frame that smuggles a chain in, as
/// `metadata.chatty.call`, does not decode at all: the codec's
/// `smuggled_chatty_call_metadata_refused_at_decode`.)
#[tokio::test]
async fn forged_chain_is_ignored() {
    let data = tempfile::tempdir().unwrap();
    let log = EdgeLog::open(data.path()).unwrap();
    let path = log.path();
    let registry = ParticipantRegistry::new();
    let spawned = Arc::new(AtomicU64::new(0));
    let runners: BTreeMap<String, Arc<dyn VirtualAgent>> = ["kit-2", "kit-5"]
        .into_iter()
        .map(|name| {
            let runner: Arc<dyn VirtualAgent> = Arc::new(Counting {
                name: name.to_string(),
                registry: registry.clone(),
                spawned: spawned.clone(),
            });
            (name.to_string(), runner)
        })
        .collect();
    let calls = BrokerCalls::new(
        registry.clone(),
        Arc::new(runners),
        Some(Arc::new(Mutex::new(log))),
    );

    // root → kit-1 → kit-2 → kit-3 → kit-4, as the runners record it.
    let mut chain = CallChain::root("t-forged");
    let mut owner: Option<String> = None;
    let mut runs = Vec::new();
    for spec in ["kit-1", "kit-2", "kit-3", "kit-4"] {
        let node = registry.admit_under(spec, owner.as_deref());
        chain = chain.extend(spec).unwrap();
        runs.push(
            registry
                .open_run(&node, owner.as_deref(), None, chain.clone())
                .expect("the node was admitted"),
        );
        owner = Some(node);
    }
    let deepest = owner.expect("four nodes");
    assert_eq!(registry.open_runs(), 4);

    for (agent, expected) in [
        ("kit-5", Refusal::TooDeep { depth: 5, max: 4 }),
        (
            "kit-2",
            Refusal::Cycle {
                chain: ["root", "kit-1", "kit-2", "kit-3", "kit-4"]
                    .map(String::from)
                    .to_vec(),
                callee: "kit-2".to_string(),
            },
        ),
    ] {
        let line = serde_json::json!({
            "v": 3, "id": 1, "method": "agent.invoke",
            "params": {
                "agent": agent, "prompt": "go on",
                "run": runs.last().expect("four runs").name(),
            }
        })
        .to_string();
        let Some(super::super::protocol::ParticipantFrame::Call { request, .. }) =
            super::super::codec::BrokerCodec::new()
                .decode(&line)
                .expect("a call frame")
        else {
            panic!("not a call frame");
        };
        let events: Vec<_> = calls.call(node(&deepest), request).collect().await;
        assert_eq!(events, [Err(CallError::Delegation(expected))], "{agent}");
    }
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "nothing was spawned");

    let rows: Vec<EdgeRow> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.kind, EdgeKind::Refusal);
        assert_eq!(row.from, deepest);
        assert_eq!(row.chain, ["root", "kit-1", "kit-2", "kit-3", "kit-4"]);
    }
    assert_eq!(rows[0].outcome, "too_deep: depth 5 > max 4");

    drop(runs);
    assert_eq!(registry.open_runs(), 0, "every run released");
}
