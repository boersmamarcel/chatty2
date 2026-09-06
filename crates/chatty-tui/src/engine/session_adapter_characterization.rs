//! The Phase 0 goldens, replayed through `AgentSession`'s handler and the
//! `From<SessionEvent> for AppEvent` adapter (AGE-194 acceptance).
//!
//! `streaming_characterization.rs` records what the TUI's own
//! `TuiStreamHandler` produces. This file records what the TUI *will* see
//! once it reparents onto the session (AGE-195), against the same goldens:
//! the adapter is one-to-one, so the two must agree — with one documented
//! exception, `provider_error_mid_stream`, asserted inline below.

use std::path::PathBuf;

use chatty_core::services::{Scenario, StreamSurface, clarification_scenario, scenarios};
use chatty_core::session::{TurnPolicy, replay_scenario};

use super::characterization::describe;
use crate::events::AppEvent;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine/goldens")
}

fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::InteractiveTui,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
    }
}

/// The golden's event lines, without the `=> loop returned …` trailer:
/// the session never propagates an error out of the loop, so the trailer
/// is not part of what the adapter can reproduce.
fn golden_events(name: &str) -> Vec<String> {
    std::fs::read_to_string(goldens_dir().join(format!("{name}.txt")))
        .expect("golden is committed")
        .lines()
        .filter(|line| !line.starts_with("=>"))
        .map(str::to_string)
        .collect()
}

async fn replay(scenario: Scenario) -> Vec<String> {
    replay_scenario(scenario, policy())
        .await
        .into_iter()
        .map(AppEvent::from)
        .map(|event| describe(&event))
        .collect()
}

#[tokio::test]
async fn session_adapter_matches_the_tui_goldens() {
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        if name == "provider_error_mid_stream" {
            continue;
        }
        assert_eq!(
            golden_events(name),
            replay(scenario).await,
            "`{name}` through the session adapter must match the Phase 0 golden"
        );
    }
}

/// The one reconciliation: a transport `Err` used to leave the loop as an
/// `Err` with no terminal event, which `send_message_inner` then reported as
/// a `StreamError` *after* `StreamCompleted`. The session reports it as a
/// typed event, in order, the way the desktop already did. AGE-195 updates
/// the golden when the TUI reparents.
#[tokio::test]
async fn a_transport_error_is_reported_before_the_turn_completes() {
    let scenario = scenarios()
        .into_iter()
        .find(|s| s.name == "provider_error_mid_stream")
        .expect("scenario exists");
    assert_eq!(
        replay(scenario).await,
        vec![
            "StreamStarted".to_string(),
            "TextChunk(\"Partial \")".to_string(),
            "StreamError(kind=Other)".to_string(),
            "StreamCompleted".to_string(),
        ]
    );
}
