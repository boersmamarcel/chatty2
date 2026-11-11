use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use chatty_fabric::CallError;
use chatty_fabric::wire::{ParticipantCard, ParticipantSkill};
use futures::StreamExt;

use super::*;

const SCOPE: &str = "conversation-1";

fn scope() -> ConversationScope {
    ConversationScope::new(SCOPE)
}

fn caller() -> ConversationScope {
    scope()
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

/// A broker whose directory is `cards`, or that does not answer once
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

    fn serve(&self, cards: Vec<AgentEntry>) {
        *self.cards.lock().unwrap() = Some(cards);
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
            .ok_or_else(|| FetchError("connection refused".into()))
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

/// A conversation's resumable handles, standing in for RC-3's index.
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

/// Fixed entries, standing in for a source a test does not exercise (and
/// for hosted agents, which no real source lists until PL-S6).
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

fn table(directory: Directory) -> Arc<dyn NodeTable> {
    Arc::new(Mutex::new(directory))
}

fn running() -> NodeState {
    NodeState::Running {
        run: serde_json::from_value(serde_json::json!(1)).unwrap(),
    }
}

/// Invariant 1: one entry of each origin, each with its reach.
#[tokio::test]
async fn roster_lists_every_origin() {
    // One running node in this conversation, one in another.
    let mut directory = Directory::new();
    let coder = directory.admit("local-coder", None, scope()).unwrap();
    directory.set_state(coder.id(), running()).unwrap();
    directory
        .admit("local-coder", None, ConversationScope::new("other"))
        .unwrap();

    let broker = FakeBroker::serving(vec![
        card("local-coder-0", "Model: qwen."),
        card("local-coder-1", "Another conversation's node."),
        card("analyst", "The broker's runner for the analyst spec."),
    ]);
    let cards = Arc::new(LiveCards::new(broker.clone()));
    let mut analyst = AgentSpec::named("analyst");
    analyst.agent.description = Some("Reads spreadsheets.".to_string());

    let roster = Roster::new(vec![
        Arc::new(RemoteSource::new(vec![
            remote("voucher", true),
            remote("off", false),
        ])),
        Arc::new(HostedSource),
        Arc::new(Fixed(vec![fixed(
            "hosted-analyst",
            Origin::Hosted,
            Reach::Startable,
        )])),
        Arc::new(LocalSpecSource::new(vec![analyst], Some(cards.clone()))),
        Arc::new(DirectorySource::new(table(directory), cards)),
        Arc::new(HandleSource::new(Arc::new(Handles(vec![(
            "reviewer-handle".to_string(),
            parked("local-coder-0"),
        )])))),
    ]);

    let entries = roster.for_caller(&CallerView { scope: &caller() }).await;

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
    assert_eq!(handle.reach, Reach::Idle, "a parked handle is idle");
    assert_eq!(handle.handle, Some(parked("local-coder-0")));

    let node = find(&entries, "local-coder-0", Origin::Node);
    assert_eq!(node.reach, Reach::Live, "a running node is live");
    assert_eq!(node.card.description, "Model: qwen.");
    assert_eq!(node.handle, None);
    assert!(
        !entries.iter().any(|entry| entry.name == "local-coder-1"),
        "another conversation's node is not on this roster: {entries:#?}"
    );

    let spec = find(&entries, "analyst", Origin::LocalSpec);
    assert_eq!(spec.reach, Reach::Startable, "a spec needs no process");
    assert_eq!(
        spec.card.description, "The broker's runner for the analyst spec.",
        "the broker's card says what the spec resolved to"
    );

    assert_eq!(
        find(&entries, "hosted-analyst", Origin::Hosted).reach,
        Reach::Startable
    );
    assert!(
        HostedSource.entries(&scope()).await.is_empty(),
        "nothing is hosted until PL-S6"
    );
    assert!(
        HandleSource::default().entries(&scope()).await.is_empty(),
        "no handles exist until RC-3"
    );

    let voucher = find(&entries, "voucher", Origin::Remote);
    assert_eq!(voucher.reach, Reach::Live);
    assert!(voucher.card.has_api_key);
    assert_eq!(voucher.card.agent_origin, AgentOrigin::RemoteConfigured);
    assert_eq!(
        find(&entries, "off", Origin::Remote).reach,
        Reach::Unreachable {
            reason: "disabled in Settings".to_string()
        }
    );
    let json = serde_json::to_string(&entries).unwrap();
    assert!(!json.contains("secret-value"), "{json}");

    assert_eq!(broker.fetches(), 1, "one roster read is one fetch");
}

/// A spec is listed with no broker at all: startable, with its own
/// description.
#[tokio::test]
async fn a_startable_spec_needs_no_broker() {
    let mut spec = AgentSpec::named("analyst");
    spec.agent.description = Some("Reads spreadsheets.".to_string());
    let roster = Roster::new(vec![Arc::new(LocalSpecSource::new(vec![spec], None))]);

    let entries = roster.for_caller(&CallerView { scope: &caller() }).await;
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
            fixed("reviewer", Origin::Hosted, Reach::Startable),
            fixed("reviewer", Origin::LocalSpec, Reach::Startable),
            fixed("reviewer", Origin::Node, Reach::Live),
        ])),
        Arc::new(HandleSource::new(Arc::new(Handles(vec![(
            "reviewer".to_string(),
            parked("root"),
        )])))),
        // A name only one origin has is nobody's loser.
        Arc::new(Fixed(vec![fixed(
            "coder",
            Origin::LocalSpec,
            Reach::Startable,
        )])),
    ]);

    let entries = roster.for_caller(&CallerView { scope: &caller() }).await;
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

    // Each step down the order: the highest origin present owns the name.
    let roster = Roster::new(vec![
        Arc::new(RemoteSource::new(vec![
            remote("a", true),
            remote("b", true),
            remote("c", true),
        ])),
        Arc::new(Fixed(vec![
            fixed("a", Origin::Node, Reach::Live),
            fixed("a", Origin::LocalSpec, Reach::Startable),
            fixed("b", Origin::LocalSpec, Reach::Startable),
            fixed("b", Origin::Hosted, Reach::Startable),
            fixed("c", Origin::Hosted, Reach::Startable),
        ])),
    ]);
    let entries = roster.for_caller(&CallerView { scope: &caller() }).await;
    let rows: Vec<(&str, Origin, Option<Origin>)> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.origin, entry.shadowed_by))
        .collect();
    assert_eq!(
        rows,
        [
            ("a", Origin::Node, None),
            ("a", Origin::LocalSpec, Some(Origin::Node)),
            ("a", Origin::Remote, Some(Origin::Node)),
            ("b", Origin::LocalSpec, None),
            ("b", Origin::Hosted, Some(Origin::LocalSpec)),
            ("b", Origin::Remote, Some(Origin::LocalSpec)),
            ("c", Origin::Hosted, None),
            ("c", Origin::Remote, Some(Origin::Hosted)),
        ]
    );
}

/// Invariant 8: a live card is fetched at most once per TTL, and a dead
/// node is unreachable after one failed fetch.
#[tokio::test]
async fn live_card_ttl() {
    let clock = FakeClock::new();
    let broker = FakeBroker::serving(vec![card("local-coder-0", "Model: qwen.")]);
    let cards = Arc::new(LiveCards::with_clock(broker.clone(), clock.clone()));
    assert_eq!(LiveCards::TTL, Duration::from_secs(30));

    let mut directory = Directory::new();
    let node = directory.admit("local-coder", None, scope()).unwrap();
    directory.set_state(node.id(), NodeState::Idle).unwrap();
    let roster = Roster::new(vec![Arc::new(DirectorySource::new(
        table(directory),
        cards,
    ))]);
    let reach = || async {
        find(
            &roster.for_caller(&CallerView { scope: &caller() }).await,
            "local-coder-0",
            Origin::Node,
        )
        .reach
        .clone()
    };

    // Within the TTL, however many reads: one fetch.
    for _ in 0..6 {
        assert_eq!(reach().await, Reach::Idle);
        clock.advance(Duration::from_secs(5));
    }
    assert_eq!(broker.fetches(), 1);

    // The node's connection goes, so the broker no longer serves its card.
    // The TTL has just run out (30 s since the fetch): the next read
    // fetches once and finds no card.
    broker.serve(Vec::new());
    let after_one_failed_fetch = reach().await;
    assert_eq!(broker.fetches(), 2);
    assert_eq!(
        after_one_failed_fetch,
        Reach::Unreachable {
            reason: "the broker serves no card for 'local-coder-0'".to_string()
        },
        "a node the broker lost is unreachable after one fetch"
    );

    // The broker itself dies: one failed fetch, once the TTL is out, and
    // the reason is the fetch's error.
    broker.serve(vec![card("local-coder-0", "Model: qwen.")]);
    clock.advance(LiveCards::TTL);
    assert_eq!(reach().await, Reach::Idle);
    assert_eq!(broker.fetches(), 3);
    broker.die();
    clock.advance(LiveCards::TTL - Duration::from_secs(1));
    assert_eq!(reach().await, Reach::Idle, "still inside the TTL");
    assert_eq!(broker.fetches(), 3);
    clock.advance(Duration::from_secs(1));
    match reach().await {
        Reach::Unreachable { reason } => {
            assert!(reason.contains("connection refused"), "{reason}")
        }
        other => panic!("a dead node is unreachable, got {other:?}"),
    }
    assert_eq!(broker.fetches(), 4, "one failed fetch is enough");

    // The failure is cached too: a dead broker is not asked again inside
    // the TTL.
    clock.advance(LiveCards::TTL - Duration::from_secs(1));
    reach().await;
    assert_eq!(broker.fetches(), 4);
    clock.advance(Duration::from_secs(1));
    reach().await;
    assert_eq!(broker.fetches(), 5);
}

/// A node still starting has not said hello, so it has no card yet and is
/// not unreachable for it; an ended node is not listed.
#[tokio::test]
async fn a_starting_node_is_live_and_an_ended_one_is_gone() {
    let mut directory = Directory::new();
    directory.admit("local-coder", None, scope()).unwrap();
    let ended = directory.admit("local-coder", None, scope()).unwrap();
    directory.end(ended.id()).unwrap();
    let cards = Arc::new(LiveCards::new(FakeBroker::serving(Vec::new())));
    let roster = Roster::new(vec![Arc::new(DirectorySource::new(
        table(directory),
        cards,
    ))]);

    let entries = roster.for_caller(&CallerView { scope: &caller() }).await;
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert_eq!(
        find(&entries, "local-coder-0", Origin::Node).reach,
        Reach::Live
    );
}

/// A broker over the fabric: its `ListAgents` answer, or why there is none.
struct BrokerOverFabric(Option<Vec<AgentEntry>>);

#[async_trait]
impl Transport for BrokerOverFabric {
    async fn call(&self, _req: CallRequest) -> Result<chatty_fabric::CallStream, CallError> {
        match &self.0 {
            Some(agents) => Ok(
                futures::stream::iter([Ok(CallEvent::Result(CallResult::Agents(agents.clone())))])
                    .boxed(),
            ),
            None => Err(CallError::Disconnected("gone".into())),
        }
    }
}

/// The real fetch reads the broker's directory over the fabric, and says
/// why when it cannot.
#[tokio::test]
async fn transport_cards_read_the_broker_s_directory() {
    let served = vec![card("local-coder-0", "Model: qwen.")];
    let fetch = TransportCards(Arc::new(BrokerOverFabric(Some(served.clone()))));
    assert_eq!(fetch.fetch().await.unwrap(), served);

    let fetch = TransportCards(Arc::new(BrokerOverFabric(None)));
    let error = fetch.fetch().await.unwrap_err();
    assert!(error.to_string().contains("gone"), "{error}");
}
