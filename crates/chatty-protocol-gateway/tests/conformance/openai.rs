//! S3 rows 3.1–3.4: the OpenAI-compatible routes.

use std::time::Duration;

use chatty_wasm_runtime::Role;
use chatty_wasm_runtime::test_support::FakeResponse;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::harness::{Gateway, Module, on_path, run_client, skip};

/// A system prompt, one earlier exchange, and a new user turn. The last user
/// message asks echo-agent to "use llm", so it forwards its whole request to
/// the fake provider: what the provider records is what the guest received.
fn history() -> Value {
    json!([
        { "role": "system", "content": "You are terse." },
        { "role": "user", "content": "first question" },
        { "role": "assistant", "content": "first answer" },
        { "role": "user", "content": "use llm: second question" },
    ])
}

/// 3.1 — `/v1/{m}/chat/completions` with system + user + assistant history:
/// every role reaches the guest as sent (F11: `system` arrives as `user`).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H4 (AGE-607)"]
async fn s3_01_openai_module_route_preserves_roles() {
    let gw = Gateway::start(
        vec![Module::shipped("echo-agent")],
        vec![FakeResponse::Text("from the model".into())],
    )
    .await;

    let (status, body) = gw
        .post(
            "/v1/echo-agent/chat/completions",
            &json!({ "model": "echo-agent", "messages": history() }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "from the model");

    let calls = gw.llm.calls();
    assert_eq!(calls.len(), 1, "echo-agent forwards the request once");
    let seen: Vec<(Role, &str)> = calls[0]
        .messages
        .iter()
        .map(|m| (m.role, m.content.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (Role::System, "You are terse."),
            (Role::User, "first question"),
            (Role::Assistant, "first answer"),
            (Role::User, "use llm: second question"),
        ],
        "the guest sees the conversation with its roles"
    );
}

/// 3.2 — the model-routed `/v1/chat/completions` with
/// `model: "module:echo-agent"` answers exactly as the module route does,
/// and a model outside the `module:` namespace is a 400.
#[tokio::test(flavor = "multi_thread")]
async fn s3_02_openai_model_routed_matches_module_route() {
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;
    let messages = json!([{ "role": "user", "content": "hello" }]);

    let (status, direct) = gw
        .post(
            "/v1/echo-agent/chat/completions",
            &json!({ "model": "echo-agent", "messages": messages }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{direct}");
    let (status, routed) = gw
        .post(
            "/v1/chat/completions",
            &json!({ "model": "module:echo-agent", "messages": messages }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{routed}");

    // Ids and timestamps differ per response; everything else must not.
    for body in [&direct, &routed] {
        assert_eq!(body["object"], "chat.completion");
        assert!(
            body["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("chatcmpl-"))
        );
    }
    for field in ["model", "choices", "usage"] {
        assert_eq!(direct[field], routed[field], "`{field}` differs");
    }
    assert_eq!(routed["choices"][0]["message"]["content"], "Echo: hello");
    assert_eq!(routed["choices"][0]["message"]["role"], "assistant");

    let (status, body) = gw
        .post(
            "/v1/chat/completions",
            &json!({ "model": "gpt-4o", "messages": messages }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// 3.3 — `stream: true`.
///
/// Decision (the plan leaves "SSE or 400" open): **400**. PL-D1 was answered
/// option B, so a module's OpenAI route is exposure only and stays
/// non-streaming; PL-H4 (Do 5) implements the 400. What must never happen is
/// what happens today: the flag is ignored and a plain JSON body comes back,
/// which a streaming client reads as an empty or broken stream.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H4 (AGE-607)"]
async fn s3_03_openai_stream_true_is_rejected_not_ignored() {
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;

    for path in ["/v1/echo-agent/chat/completions", "/v1/chat/completions"] {
        let resp = gw
            .http
            .post(gw.url(path))
            .json(&json!({
                "model": "module:echo-agent",
                "stream": true,
                "messages": [{ "role": "user", "content": "hello" }],
            }))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = resp.text().await.unwrap();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{path}: `stream: true` answered {status} ({content_type}): {body}"
        );
        assert!(body.contains("stream"), "{path}: the 400 says why: {body}");
    }
}

/// The official OpenAI Python client, against both OpenAI routes.
const OPENAI_CLIENT: &str = r#"
import json, sys
from openai import OpenAI

base = sys.argv[1]
messages = [
    {"role": "system", "content": "You are terse."},
    {"role": "user", "content": "hi from python"},
]
module = OpenAI(base_url=base + "/v1/echo-agent", api_key="unused", max_retries=0, timeout=30)
routed = OpenAI(base_url=base + "/v1", api_key="unused", max_retries=0, timeout=30)
a = module.chat.completions.create(model="echo-agent", messages=messages)
b = routed.chat.completions.create(model="module:echo-agent", messages=messages)
print(json.dumps({
    "module": a.choices[0].message.content,
    "routed": b.choices[0].message.content,
    "role": a.choices[0].message.role,
    "finish_reason": a.choices[0].finish_reason,
    "object": a.object,
    "model": b.model,
}))
"#;

/// 3.4 — the official `openai` Python package, through `uv`, end to end on
/// both routes. Skips (printing why) when `uv` is not installed; CI installs
/// it. `uv` fetches the package from PyPI on first use.
#[tokio::test(flavor = "multi_thread")]
async fn s3_04_openai_python_client_end_to_end() {
    let Some(uv) = on_path("uv") else {
        skip(
            "3.4 s3_04_openai_python_client_end_to_end",
            "`uv` is not on PATH, so the official OpenAI Python client cannot run",
        );
        return;
    };
    let gw = Gateway::start(vec![Module::shipped("echo-agent")], vec![]).await;

    let mut command = tokio::process::Command::new(uv);
    command.args([
        "run",
        "--no-project",
        "--quiet",
        "--with",
        "openai>=1,<3",
        "python",
        "-c",
        OPENAI_CLIENT,
        &gw.base,
    ]);
    let output = run_client("3.4", command, Duration::from_secs(300)).await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Value = serde_json::from_str(stdout.trim().lines().last().unwrap_or(""))
        .unwrap_or_else(|e| panic!("3.4: the client printed no JSON ({e}): {stdout}"));
    assert_eq!(result["module"], "Echo: hi from python", "{result}");
    assert_eq!(result["routed"], "Echo: hi from python", "{result}");
    assert_eq!(result["role"], "assistant");
    assert_eq!(result["finish_reason"], "stop");
    assert_eq!(result["object"], "chat.completion");
    assert_eq!(result["model"], "module:echo-agent");
}
