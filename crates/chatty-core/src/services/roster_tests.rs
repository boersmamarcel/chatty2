use std::sync::atomic::{AtomicUsize, Ordering};

use chatty_fabric::wire::{ParticipantCard, ParticipantSkill};

use super::*;

const SCOPE: &str = "conversation-1";

fn scope() -> ConversationScope {
    ConversationScope::new(SCOPE)
}

/// A clock a test moves by hand.
struct FakeClock(Mutex<Instant>);

impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Instant::now())))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

/// A broker whose directory is `cards`, or who does not answer once
/// `cards` is `None`; counts its fetches.
struct FakeBroker {
    cards: Mutex<Option<Vec<AgentEntry>>>,
    fetches: AtomicUsize,
}

impl FakeBroker {
    fn serving(cards: Vec<AgentEntry>) -> Arc<Self> {
        Arc::new(Self {
            cards: Mutex::new(Some(cards)),
            fetches: AtomicUsize::new(0),
        })
    }

    fn die(&self) {
        *self.cards.lock().unwrap() = None;
    }

    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CardFetch for FakeBroker {
    async fn fetch(&self) -> Result<Vec<AgentEntry>, FetchError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.cards
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| FetchError::NoAnswer("connection refused".into()))
    }
}

fn card(name: &str, description: &str) -> AgentEntry {
    AgentEntry::from_card(
        &ParticipantCard {
            name: name.to_string(),
            description: description.to_string(),
            skills: vec![ParticipantSkill {
                name: "delegate".to_string(),
                ..ParticipantSkill::default()
            }],
            ..ParticipantCard::default()
        },
        AgentOrigin::Local,
    )
}

fn remote(name: &str, enabled: bool) -> A2aAgentConfig {
    A2aAgentConfig {
        name: name.to_string(),
        url: format!("https://example.com/a2a/{name}"),
        api_key: Some("secret-value".to_string()),
        enabled,
        skills: vec![],
        allow_private_network: false,
    }
}

struct Handles(Vec<(String, HandleInfo)>);

impl HandleIndex for Handles {
    fn handles(&self, scope: &ConversationScope) -> Vec<(String, HandleInfo)> {
        if scope.as_str() == SCOPE {
            self.0.clone()
        } else {
            Vec::new()
        }
    }
}

fn parked(owner: &str) -> HandleInfo {
    HandleInfo {
        owner: owner.to_string(),
        state: HandleState::Parked,
        pending_messages: 2,
        behind_by: Some(1),
        idle_for_s: 120,
    }
}

/// Entries of fixed origins, standing in for a source this test does not
/// exercise.
struct Fixed(Vec<RosterEntry>);

#[async_trait]
impl RosterSource for Fixed {
    async fn entries(&self, _scope: &ConversationScope) -> Vec<RosterEntry> {
        self.0.clone()
    }
}

fn fixed(name: &str, origin: Origin, reach: Reach) -> RosterEntry {
    RosterEntry::new(name, origin, reach, RosterCard::local(origin.as_str()))
}

fn find<'a>(entries: &'a [RosterEntry], name: &str, origin: Origin) -> &'a RosterEntry {
    entries
        .iter()
        .find(|entry| entry.name == name && entry.origin == origin)
        .unwrap_or_else(|| panic!("{name} ({origin}) is on the roster: {entries:#?}"))
}

/// Invariant 1: one entry of each origin, each with its reach.
#[tokio::test]
async fn roster_lists_every_origin() {
    // A directory with one running node and one idle node in this
    // conversation, and one in another conversation.
    let mut directory = Directory::new();
    let coder = directory.admit("local-coder", None, scope()).unwrap();
    directory
        .set_state(
            coder.id(),
            NodeState::Running {
                run: serde_json::from_value(serde_json::json!(1)).unwrap(),
            },
        )
        .unwrap();
    directory
        .admit("local-coder", None, ConversationScope::new("other"))
        .unwrap();
    let directory: Arc<dyn NodeTable> = Arc::new(Mutex::new(directory));

    let broker = FakeBroker::serving(vec![
        card("local-coder-0", "Model: qwen."),
        card("local-coder-1", "Another conversation's node."),
        card("analyst", "The broker's runner for the analyst spec."),
    ]);
    let cards = Arc::new(LiveCards::new(broker.clone()));
    let mut analyst = AgentSpec::named("analyst");
    analyst.agent.description = Some("Reads spreadsheets.".to_string());

    let local = LocalSpecSource::new(vec![analyst], Some(cards.clone()));
    let nodes = DirectorySource::new(cards, local.names().map(str::to_string).collect::<Vec<_>>())
        .with_table(directory);
    let handles = HandleSource::new(Arc::new(Handles(vec![(
        "reviewer-handle".to_string(),
        parked("local-coder-0"),
    )])));
    let roster = Roster::new(vec![
        Arc::new(RemoteSource::new(vec![remote("voucher", true), remote("off", false)])),
        Arc::new(HostedSource),
        Arc::new(Fixed(vec![fixed("hosted-analyst", Origin::Hosted, Reach::Live)])),
        Arc::new(local),
        Arc::new(nodes),
        Arc::new(handles),
    ]);

    let entries = roster.for_caller(&CallerView { scope: &scope() }).await;

    let origins: BTreeSet<Origin> = entries.iter().map(|entry| entry.origin).collect();
    assert_eq!(
        origins,
        BTreeSet::from([
            Origin::Handle,
            Origin::Node,
            Origin::LocalSpec,
            Origin::Hosted,
            Origin::Remote
        ])
    );
    assert!(entries.iter().all(|entry| entry.shadowed_by.is_none()));

    let handle = find(&entries, "reviewer-handle", Origin::Handle);
    assert_eq!(handle.reach, Reach::Idle);
    assert_eq!(handle.handle, Some(parked("local-coder-0")));

    let node = find(&entries, "local-coder-0", Origin::Node);
    assert_eq!(node.reach, Reach::Live);
    assert_eq!(node.card.description, "Model: qwen.");
    assert!(
        !entries.iter().any(|entry| entry.name == "local-coder-1"),
        "another conversation's node is not on this roster: {entries:#?}"
    );

    let spec = find(&entries, "analyst", Origin::LocalSpec);
    assert_eq!(spec.reach, Reach::Startable);
    assert_eq!(
        spec.card.description, "The broker's runner for the analyst spec.",
        "the broker's card says what the spec resolved to"
    );
    assert_eq!(
        entries.iter().filter(|entry| entry.name == "analyst").count(),
        1,
        "the broker's runner for a spec is the spec, not a node"
    );

    assert_eq!(
        find(&entries, "hosted-analyst", Origin::Hosted).reach,
        Reach::Live
    );
    assert!(HostedSource.entries(&scope()).await.is_empty());

    let voucher = find(&entries, "voucher", Origin::Remote);
    assert_eq!(voucher.reach, Reach::Live);
    assert!(voucher.card.has_api_key);
    assert_eq!(voucher.card.agent_origin, AgentOrigin::RemoteConfigured);
    assert!(matches!(
        find(&entries, "off", Origin::Remote).reach,
        Reach::Unreachable { .. }
    ));
    let json = serde_json::to_string(&entries).unwrap();
    assert!(!json.contains("secret-value"), "{json}");

    assert_eq!(broker.fetches(), 1, "one roster read is one fetch");
}

/// A spec needs no broker to be listed: startable, with its spec's own
/// description.
#[tokio::test]
async fn a_startable_spec_needs_no_broker() {
    let mut spec = AgentSpec::named("analyst");
    spec.agent.description = Some("Reads spreadsheets.".to_string());
    let roster = Roster::new(vec![Arc::new(LocalSpecSource::new(vec![spec], None))]);

    let entries = roster.for_caller(&CallerView { scope: &scope() }).await;
    let entry = find(&entries, "analyst", Origin::LocalSpec);
    assert_eq!(entry.reach, Reach::Startable);
    assert_eq!(entry.card.description, "Reads spreadsheets.");
}

/// Invariant 2: on a shared name the higher origin owns it, and every
/// lower one stays listed, shadowed by it.
#[tokio::test]
async fn name_collision_shadows_not_hides() {
    // All five origins claim "reviewer", from sources in the reverse of
    // precedence order, so the order sources are read in decides nothing.
    let roster = Roster::new(vec![
        Arc::new(RemoteSource::new(vec![remote("reviewer", true)])),
        Arc::new(Fixed(vec![
            fixed("reviewer", Origin::Hosted, Reach::Live),
            fixed("reviewer", Origin::LocalSpec, Reach::Startable),
            fixed("reviewer", Origin::Node, Reach::Live),
        ])),
        Arc::new(HandleSource::new(Arc::new(Handles(vec![(
            "reviewer".to_string(),
            parked("root"),
        )])))),
        // A name only one origin has is nobody's loser.
        Arc::new(Fixed(vec![fixed("coder", Origin::LocalSpec, Reach::Startable)])),
    ]);

    let entries = roster.for_caller(&CallerView { scope: &scope() }).await;
    let reviewer: Vec<(Origin, Option<Origin>)> = entries
        .iter()
        .filter(|entry| entry.name == "reviewer")
        .map(|entry| (entry.origin, entry.shadowed_by))
        .collect();
    assert_eq!(
        reviewer,
        [
            (Origin::Handle, None),
            (Origin::Node, Some(Origin::Handle)),
            (Origin::LocalSpec, Some(Origin::Handle)),
            (Origin::Hosted, Some(Origin::Handle)),
            (Origin::Remote, Some(Origin::Handle)),
        ]
    );
    assert_eq!(find(&entries, "coder", Origin::LocalSpec).shadowed_by, None);

    // Without the handle, the node owns the name; a remote under a local
    // spec is shadowed by the spec.
    let roster = Roster::new(vec![
        Arc::new(RemoteSource::new(vec![remote("reviewer", true), remote("coder", true)])),
        Arc::new(Fixed(vec![
            fixed("reviewer", Origin::Node, Reach::Live),
            fixed("reviewer", Origin::LocalSpec, Reach::Startable),
            fixed("coder", Origin::LocalSpec, Reach::Startable),
        ])),
    ]);
    let entries = roster.for_caller(&CallerView { scope: &scope() }).await;
    assert_eq!(
        find(&entries, "reviewer", Origin::Remote).shadowed_by,
        Some(Origin::Node)
    );
    assert_eq!(
        find(&entries, "reviewer", Origin::LocalSpec).shadowed_by,
        Some(Origin::Node)
    );
    assert_eq!(
        find(&entries, "coder", Origin::Remote).shadowed_by,
        Some(Origin::LocalSpec)
    );
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["coder", "coder", "reviewer", "reviewer", "reviewer"]);
}

/// Invariant 8: a live card is fetched at most once per TTL, and a node
/// whose broker stops answering is unreachable after one failed fetch.
#[tokio::test]
async fn live_card_ttl() {
    let clock = FakeClock::new();
    let broker = FakeBroker::serving(vec![card("local-coder-0", "Model: qwen.")]);
    let cards = Arc::new(LiveCards::with_clock(
        broker.clone(),
        clock.clone(),
        LiveCards::TTL,
    ));

    let mut directory = Directory::new();
    let node = directory.admit("local-coder", None, scope()).unwrap();
    directory.set_state(node.id(), NodeState::Idle).unwrap();
    let table: Arc<dyn NodeTable> = Arc::new(Mutex::new(directory));
    let roster = Roster::new(vec![Arc::new(
        DirectorySource::new(cards, Vec::new()).with_table(table),
    )]);
    let read = || async { roster.for_caller(&CallerView { scope: &scope() }).await };

    // Within the TTL, however many reads: one fetch.
    for _ in 0..5 {
        let entries = read().await;
        assert_eq!(find(&entries, "local-coder-0", Origin::Node).reach, Reach::Idle);
        clock.advance(Duration::from_secs(5));
    }
    assert_eq!(broker.fetches(), 1);

    // The node dies. The cached card still answers until the TTL runs out…
    broker.die();
    clock.advance(LiveCards::TTL - Duration::from_secs(26));
    assert_eq!(
        find(&read().await, "local-coder-0", Origin::Node).reach,
        Reach::Idle
    );
    assert_eq!(broker.fetches(), 1);

    // …then one failed fetch makes it unreachable, with the reason.
    clock.advance(Duration::from_secs(1));
    let entries = read().await;
    match &find(&entries, "local-coder-0", Origin::Node).reach {
        Reach::Unreachable { reason } => assert!(reason.contains("connection refused"), "{reason}"),
        other => panic!("a dead node is unreachable, got {other:?}"),
    }
    assert_eq!(broker.fetches(), 2);

    // The failure is cached too: the dead broker is not asked again inside
    // the TTL.
    read().await;
    clock.advance(Duration::from_secs(29));
    read().await;
    assert_eq!(broker.fetches(), 2);
    clock.advance(Duration::from_secs(1));
    read().await;
    assert_eq!(broker.fetches(), 3);
}

/// A node the broker answers for but serves no card of — its connection
/// is gone — is unreachable; an ended node is not listed.
#[tokio::test]
async fn a_node_with_no_card_is_unreachable_and_an_ended_one_is_gone() {
    let mut directory = Directory::new();
    let alive = directory.admit("local-coder", None, scope()).unwrap();
    directory.set_state(alive.id(), NodeState::Idle).unwrap();
    let ended = directory.admit("local-coder", None, scope()).unwrap();
    directory.end(ended.id()).unwrap();
    let table: Arc<dyn NodeTable> = Arc::new(Mutex::new(directory));
    let cards = Arc::new(LiveCards::new(FakeBroker::serving(Vec::new())));
    let roster = Roster::new(vec![Arc::new(
        DirectorySource::new(cards, Vec::new()).with_table(table),
    )]);

    let entries = roster.for_caller(&CallerView { scope: &scope() }).await;
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert!(matches!(
        find(&entries, "local-coder-0", Origin::Node).reach,
        Reach::Unreachable { .. }
    ));
}
