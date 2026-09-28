//! Test assets for suites that load real WASM modules (AGE-596).
//!
//! * [`fixture_path`] finds a module staged by `scripts/build-wasm-fixtures.sh`.
//! * [`FakeLlm`] is an [`LlmProvider`] that records every `llm::complete` call
//!   and replays a script of responses.
//!
//! Only compiled with the `test-support` feature.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::LlmProvider;
use crate::bindings::chatty::plugin::types::{CompletionResponse, Message, ToolCall};

/// Path of the staged `.wasm` for the fixture `name` (e.g. `"spin"`,
/// `"echo-agent"`): `target/wasm-fixtures/<name>/<name>.wasm`. Its directory
/// also holds the module's `module.toml`, so it loads through the registry too.
///
/// # Panics
/// When the fixture has not been built, naming the script that builds it:
/// the `.wasm` files are build output, not checked in, and a test that
/// silently skipped would pass while testing nothing.
pub fn fixture_path(name: &str) -> PathBuf {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("chatty-wasm-runtime lives two levels below the workspace root");
    let dir = workspace.join("target/wasm-fixtures").join(name);
    let wasm = dir.join(format!("{name}.wasm"));
    if !wasm.is_file() || !dir.join("module.toml").is_file() {
        panic!(
            "WASM fixture `{name}` not found at {}\n\
             Fixtures are build output; build them once per checkout:\n  \
             cd {} && scripts/build-wasm-fixtures.sh",
            wasm.display(),
            workspace.display()
        );
    }
    wasm
}

/// One scripted reply of a [`FakeLlm`].
#[derive(Debug, Clone)]
pub enum FakeResponse {
    /// A plain text completion.
    Text(String),
    /// A completion that asks for these tool calls, with no text.
    ToolCalls(Vec<ToolCall>),
    /// The provider fails with this message.
    Err(String),
    /// Sleep on the calling (guest's) thread, then reply with this text.
    Delay(Duration, String),
}

/// One `llm::complete` call a [`FakeLlm`] received.
#[derive(Debug, Clone)]
pub struct FakeLlmCall {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Option<String>,
}

/// An in-process [`LlmProvider`] for the module host's `llm::complete` path:
/// replays its script in order, one response per call, and records every
/// call. A call past the end of the script is an error naming the count, so a
/// guest that calls more often than the test expected fails loudly.
#[derive(Debug, Default)]
pub struct FakeLlm {
    script: Mutex<VecDeque<FakeResponse>>,
    calls: Mutex<Vec<FakeLlmCall>>,
}

impl FakeLlm {
    pub fn new(script: impl IntoIterator<Item = FakeResponse>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::default(),
        }
    }

    /// Every call received so far, oldest first.
    pub fn calls(&self) -> Vec<FakeLlmCall> {
        self.calls.lock().expect("FakeLlm calls lock").clone()
    }
}

impl LlmProvider for FakeLlm {
    fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        let call_count = {
            let mut calls = self.calls.lock().expect("FakeLlm calls lock");
            calls.push(FakeLlmCall {
                model: model.to_string(),
                messages,
                tools,
            });
            calls.len()
        };
        let next = self.script.lock().expect("FakeLlm script lock").pop_front();
        let text = |content: String| CompletionResponse {
            content,
            tool_calls: vec![],
            usage: None,
        };
        match next {
            Some(FakeResponse::Text(content)) => Ok(text(content)),
            Some(FakeResponse::ToolCalls(tool_calls)) => Ok(CompletionResponse {
                content: String::new(),
                tool_calls,
                usage: None,
            }),
            Some(FakeResponse::Err(message)) => Err(message),
            Some(FakeResponse::Delay(delay, content)) => {
                std::thread::sleep(delay);
                Ok(text(content))
            }
            None => Err(format!("FakeLlm: script exhausted at call {call_count}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::chatty::plugin::types::Role;

    fn user(content: &str) -> Vec<Message> {
        vec![Message {
            role: Role::User,
            content: content.to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        }]
    }

    #[test]
    fn fake_llm_replays_script_and_records_calls() {
        let call = ToolCall {
            id: "1".into(),
            name: "echo".into(),
            arguments: "{}".into(),
        };
        let llm = FakeLlm::new([
            FakeResponse::Text("hi".into()),
            FakeResponse::ToolCalls(vec![call]),
            FakeResponse::Err("boom".into()),
            FakeResponse::Delay(Duration::from_millis(20), "late".into()),
        ]);

        assert_eq!(llm.complete("m", user("a"), None).unwrap().content, "hi");
        let tools = llm.complete("m", user("b"), Some("[]".into())).unwrap();
        assert_eq!(tools.tool_calls[0].name, "echo");
        assert_eq!(llm.complete("m", user("c"), None).unwrap_err(), "boom");
        let start = std::time::Instant::now();
        assert_eq!(llm.complete("m", user("d"), None).unwrap().content, "late");
        assert!(start.elapsed() >= Duration::from_millis(20));
        let err = llm.complete("m", user("e"), None).unwrap_err();
        assert!(err.contains("exhausted at call 5"), "{err}");

        let calls = llm.calls();
        assert_eq!(calls.len(), 5);
        assert_eq!(calls[1].tools.as_deref(), Some("[]"));
        assert_eq!(calls[3].messages[0].content, "d");
    }

    #[test]
    fn missing_fixture_names_the_build_script() {
        let panic = std::panic::catch_unwind(|| fixture_path("no-such-fixture"))
            .expect_err("a missing fixture must panic");
        let message = panic.downcast_ref::<String>().expect("formatted panic");
        assert!(message.contains("no-such-fixture"), "{message}");
        assert!(
            message.contains("scripts/build-wasm-fixtures.sh"),
            "{message}"
        );
    }
}
