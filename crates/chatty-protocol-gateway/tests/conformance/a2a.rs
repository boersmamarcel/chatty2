//! S3 row 3.7: A2A `message/send` delivers every text part.
//!
//! The A2A route serves agents (local participants and virtual agents),
//! never a plugin (PL-U3), so the row runs against a participant; what it
//! pins is the gateway's own part handling, which is the same for every
//! agent. The module rows that were here — per-`contextId` history for a
//! module (3.7), a module's stream lifecycle (3.8) and a disconnect during a
//! module's call (3.9) — went with `chat`. A participant's stream lifecycle
//! and a worker dying mid-task are `participant_socket.rs`'s
//! `a_registered_participant_round_trips_a_task_with_progress_and_an_artifact`
//! and `a_participant_that_dies_mid_task_fails_its_open_task`.

/// 3.7 — `message/send` with a multi-part message: every text part reaches
/// the agent, not only `parts[0]` (F11).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn s3_07_a2a_message_send_all_parts_reach_the_agent() {
    use chatty_protocol_gateway::participant::{
        BrokerFrame, ParticipantCard, ParticipantConnection, TaskState, open_connection,
    };
    use reqwest::StatusCode;
    use serde_json::json;

    use crate::harness::Gateway;

    let gw = Gateway::start(vec![], vec![]).await;
    let connection = open_connection(&gw.participants, "parts").unwrap();
    connection.worker_end.set_nonblocking(true).unwrap();
    let stream = tokio::net::UnixStream::from_std(connection.worker_end).unwrap();
    let card = ParticipantCard {
        name: "parts".into(),
        ..Default::default()
    };
    let mut conn = ParticipantConnection::hello_over(stream, card)
        .await
        .expect("the broker welcomes the participant");
    let name = conn.name().to_string();
    let participant = tokio::spawn(async move {
        let Some(BrokerFrame::Task { task_id, text, .. }) = conn.next_frame().await.unwrap() else {
            panic!("expected a task");
        };
        conn.artifact(&task_id, "ok".to_string(), true)
            .await
            .unwrap();
        conn.finish(&task_id, TaskState::Completed, None, None)
            .await
            .unwrap();
        (conn, text)
    });

    let (status, body) = gw
        .post(
            &format!("/a2a/{name}"),
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "message/send",
                "params": {
                    "message": {
                        "role": "user",
                        "messageId": "m1",
                        "parts": [
                            { "kind": "text", "text": "part one" },
                            { "kind": "text", "text": "part two" },
                        ],
                    }
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["status"]["state"], "completed", "{body}");

    let (_conn, seen) = participant.await.unwrap();
    for part in ["part one", "part two"] {
        assert!(
            seen.contains(part),
            "`{part}` did not reach the agent: {seen:?}"
        );
    }
}
