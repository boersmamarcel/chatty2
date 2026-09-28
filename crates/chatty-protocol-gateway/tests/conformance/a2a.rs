//! S3 row 3.7: A2A `message/send` against a role.
//!
//! Before BI-7, the A2A route served agents (local participants and virtual
//! agents), never a plugin (PL-U3), and this row pinned the gateway's own
//! multi-part message handling on that path. Since BI-7 a role is reached
//! only over the connection the broker made for its caller, never over
//! loopback HTTP: `message/send` against a registered participant is
//! refused with 403, exactly as it is against any other role
//! (`crates/chatty-protocol-gateway/tests/loopback_scope.rs`'s
//! `loopback_refuses_roles` is the general form). The multi-part parsing
//! this row used to exercise (`prompt_text`) went with the HTTP path itself
//! — there is nothing left on this route to join parts for. The module rows
//! that were here — per-`contextId` history for a module (3.7), a module's
//! stream lifecycle (3.8) and a disconnect during a module's call (3.9) —
//! went with `chat`. A participant's stream lifecycle and a worker dying
//! mid-task, over the connection, are `participant::swarm_kit`'s coverage
//! now; the loopback round trip this row itself drove is retired with it.

/// 3.7 — `message/send` against a role (a multi-part message, to keep the
/// row's original shape) is refused: a role answers over its own connection
/// now, never over loopback (BI-7).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn s3_07_a2a_message_send_to_a_role_is_refused() {
    use chatty_protocol_gateway::participant::{
        ParticipantCard, ParticipantConnection, open_connection,
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
    let conn = ParticipantConnection::hello_over(stream, card)
        .await
        .expect("the broker welcomes the participant");
    let name = conn.name().to_string();

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
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["error"], "fabric: roles are reached over the worker connection",
        "{body}"
    );

    drop(conn);
}
