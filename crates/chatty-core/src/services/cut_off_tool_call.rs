//! A tool call whose JSON arguments stopped before they were complete
//! (AGE-983).
//!
//! A local model writing a whole 43-section report as the one `content`
//! argument of a `write_file` call stopped partway through that string. rig
//! 0.42 reported it as `tool call `write_file` arrived with malformed JSON
//! input: EOF while parsing a string at line 1 column 13114` and ended the
//! run, and the delegated worker's whole invocation with it. Sending the
//! same request again produced the same cut-off. rig 0.46 offers such a call
//! to the invalid-call hooks instead; [`CutOffCalls`] keeps it and ends the
//! run as 0.42 did, so the answer below stays the same (AGE-1151).
//!
//! `stream_prompt` answers such a call itself instead: it hands the model an
//! error tool result for the call ([`CutOffToolCall::tool_result`]), naming
//! the tool and the column and saying how to write in parts, and continues
//! the run from the messages its last request carried (`RequestRecorder`)
//! plus that call and its result. The model meets the failure where it
//! would have met the tool's output. It does so at most
//! [`MAX_CUT_OFF_RECOVERIES`] times per turn. The next cut-off ends the turn
//! with rig's error, classified as a malformed tool call.
//!
//! # Why the arguments stop
//!
//! rig 0.42 raised this error only for a call the provider said was complete:
//! `finish_reason` `tool_calls`, `stop` or an unknown reason, or a bare
//! `[DONE]` (rig-core 0.42 `openai_chat_completions_compatible.rs`). A call
//! cut off at the output cap (`finish_reason: length`) is dropped silently
//! instead, and the AGE-967 cut-off handling (`EmptyTurnRetry`, `ReplyRoom`,
//! `CUT_OFF_FOLLOW_UP`) takes over. So this is not the output cap. Either
//! the model ended its turn inside the string (an end-of-turn token sampled
//! mid-argument, which vLLM reports as `stop`), or the server's tool parser
//! closed the call before streaming the rest of its arguments (`tool_calls`).
//! The log line at the recovery point gives the argument length and its last
//! characters. That helps tell the two apart.

use std::sync::Arc;

use parking_lot::Mutex;
use rig_agent::agent::{
    AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason,
};
use rig_core::completion::Message;
use rig_core::message::{AssistantContent, CallId, ToolName, ToolResultContent, UserContent};

/// `tool` as a rig tool name; a call always names its tool.
fn tool_name(tool: &str) -> ToolName {
    ToolName::new(tool).unwrap_or_else(|_| ToolName::new("unnamed_tool").expect("a non-empty name"))
}

/// How many cut-off tool calls one turn answers with an error tool result
/// before the next one ends the turn.
pub const MAX_CUT_OFF_RECOVERIES: usize = 3;

/// How many trailing characters of a call's arguments the log line keeps.
const TAIL_CHARS: usize = 160;

/// A tool call rig rejected because its arguments did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutOffToolCall {
    /// The tool the model called.
    pub tool: String,
    /// serde's parse error, e.g. `EOF while parsing a string at line 1
    /// column 13114`.
    pub reason: String,
}

impl CutOffToolCall {
    /// The error tool result the model gets for the call. It starts with
    /// `Error:`, which is what marks a tool result as a failure
    /// (`tool_result_looks_like_error`).
    pub fn tool_result(&self) -> String {
        format!(
            "Error: {}: this call was cut off before its arguments were complete ({}), so it \
             did not run and nothing was written. Do not send the same long call again. Write \
             large content in parts: write the first part with write_file, then add each \
             further part with write_file and \"append\": true, each under about 8 KB, or \
             split it into several smaller files. Otherwise shorten the arguments.",
            self.tool, self.reason
        )
    }

    /// The call as the model made it, with `said` (the text of the same
    /// model call, if any) ahead of it and empty arguments, since its own
    /// never parsed. Also the user message carrying its error result.
    /// `call_id` pairs them.
    pub fn messages(&self, call_id: &str, said: &str) -> (Message, Message) {
        let mut content = Vec::new();
        if !said.trim().is_empty() {
            content.push(AssistantContent::text(said));
        }
        let name = tool_name(&self.tool);
        content.push(AssistantContent::tool_call(
            call_id,
            name.clone(),
            serde_json::json!({}),
        ));
        let call = Message::Assistant(rig_core::message::AssistantMessage::new(content));
        let result = Message::User {
            content: vec![UserContent::tool_result(
                CallId::from_wire(call_id),
                name,
                vec![ToolResultContent::text(self.tool_result())],
            )],
        };
        (call, result)
    }
}

/// Ends a run on a tool call whose arguments never parsed, keeping the call
/// for `stream_prompt`, which answers it ([`CutOffToolCall::messages`]) and
/// continues the turn. A run hook, so the agent's own invalid-call hooks
/// (name repair, tool loading, the turn budget's wrap-up) answer first.
/// Clones share the kept call.
#[derive(Clone, Debug, Default)]
pub struct CutOffCalls(Arc<Mutex<Option<CutOffToolCall>>>);

impl CutOffCalls {
    /// The call the run that just failed ended on, if it ended on one.
    pub fn take(&self) -> Option<CutOffToolCall> {
        self.0.lock().take()
    }
}

impl AgentHook for CutOffCalls {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        let InvalidToolCallReason::MalformedArguments { error } = &event.reason else {
            return None;
        };
        *self.0.lock() = Some(CutOffToolCall {
            tool: event.tool_name.clone(),
            reason: error.clone(),
        });
        Some(InvalidToolCallAction::fail())
    }
}

/// The arguments streamed so far for the tool call in flight: their length
/// and their last [`TAIL_CHARS`] characters, for the log line at a cut-off.
#[derive(Debug, Default)]
pub struct ArgumentTail {
    tail: String,
    chars: usize,
}

impl ArgumentTail {
    /// A new call has started.
    pub fn clear(&mut self) {
        self.tail.clear();
        self.chars = 0;
    }

    /// One more fragment of the call's arguments.
    pub fn push(&mut self, delta: &str) {
        self.chars += delta.chars().count();
        self.tail.push_str(delta);
        let excess = self.tail.chars().count().saturating_sub(TAIL_CHARS);
        if excess > 0 {
            let cut = self
                .tail
                .char_indices()
                .nth(excess)
                .map_or(self.tail.len(), |(at, _)| at);
            self.tail.drain(..cut);
        }
    }

    /// The last characters of the arguments.
    pub fn tail(&self) -> &str {
        &self.tail
    }

    /// How many characters the arguments had.
    pub fn chars(&self) -> usize {
        self.chars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The result names the tool and the column, says how to write in
    /// parts, and reads as a failed tool result.
    #[test]
    fn the_result_says_how_to_write_in_parts() {
        let cut = CutOffToolCall {
            tool: "compile_typst".into(),
            reason: "EOF while parsing a string at line 1 column 1338".into(),
        };
        let text = cut.tool_result();
        assert!(crate::services::llm_service::tool_result_looks_like_error(
            &text
        ));
        assert!(text.starts_with("Error: compile_typst:"), "{text}");
        assert!(text.contains("column 1338"), "{text}");
        assert!(text.contains("\"append\": true"), "{text}");
        assert!(text.contains("shorten"), "{text}");
    }

    /// The pair is a call and its answer, under one id.
    #[test]
    fn the_messages_pair_the_call_with_its_result() {
        let cut = CutOffToolCall {
            tool: "write_file".into(),
            reason: "EOF while parsing a string at line 1 column 8143".into(),
        };
        let (call, result) = cut.messages("call_cut_off_1", "Writing the report.");
        let Message::Assistant(rig_core::message::AssistantMessage { content, .. }) = call else {
            panic!("an assistant message")
        };
        assert_eq!(content.len(), 2, "the text, then the call");
        let AssistantContent::ToolCall(tool_call) = &content[1] else {
            panic!("a tool call")
        };
        assert_eq!(tool_call.function.name, "write_file");
        assert!(tool_call.function.arguments.is_empty());
        let Message::User { content } = result else {
            panic!("a user message")
        };
        let UserContent::ToolResult(tool_result) = &content[0] else {
            panic!("a tool result")
        };
        assert_eq!(tool_result.call, tool_call.id);
        assert_eq!(tool_result.name, "write_file");

        let (call, _) = cut.messages("call_cut_off_2", "  ");
        let Message::Assistant(rig_core::message::AssistantMessage { content, .. }) = call else {
            panic!("an assistant message")
        };
        assert_eq!(content.len(), 1, "no empty text block");
    }

    #[test]
    fn the_tail_keeps_the_last_characters() {
        let mut tail = ArgumentTail::default();
        let head = "{\"path\": \"report.typ\", \"content\": \"";
        tail.push(head);
        for _ in 0..100 {
            tail.push("= Sectión ");
        }
        assert_eq!(tail.chars(), head.chars().count() + 1000);
        assert_eq!(tail.tail().chars().count(), TAIL_CHARS);
        assert!(tail.tail().ends_with("= Sectión "));
        tail.clear();
        assert_eq!((tail.tail(), tail.chars()), ("", 0));
    }

    // ── Against the fake model ───────────────────────────────────────────

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::agent_spec::AgentSpec;
    use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
    use crate::services::{StreamErrorKind, StreamSurface};
    use crate::session::{
        AgentSession, AgentSessionConfig, MALFORMED_TOOL_CALL_FOLLOW_UP, SessionEvent, TurnInput,
    };
    use crate::settings::models::execution_settings::ExecutionSettingsModel;
    use crate::settings::models::models_store::ModelConfig;
    use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
    use crate::testing::fake_model::{FakeDaemon, Reply, Script};

    const MODEL: &str = "age-983-model";

    /// A headless worker on an OpenAI-compatible model behind `daemon`,
    /// with its workspace (kept alive by the caller).
    async fn worker(daemon: &FakeDaemon) -> (AgentSession, tempfile::TempDir) {
        let _ = crate::init_repositories();
        let model = ModelConfig::new(
            MODEL.to_string(),
            MODEL.to_string(),
            ProviderType::OpenRouter,
            MODEL.to_string(),
        );
        let provider = ProviderConfig::new("vllm".to_string(), ProviderType::OpenRouter)
            .with_api_key("unused".to_string())
            .with_base_url(daemon.base_url());
        let workspace = tempfile::tempdir().expect("a workspace");
        let settings = ExecutionSettingsModel {
            workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
            fetch_enabled: false,
            ..ExecutionSettingsModel::default()
        };
        let built = AgentBuildContext::from_spec(
            &AgentSpec::named("typesetter-like"),
            AgentServices {
                exec_settings: Some(settings.clone()),
                ..AgentServices::default()
            },
        )
        .expect("the spec builds");
        let mut session = AgentSession::new(AgentSessionConfig {
            execution_settings: settings,
            surface: StreamSurface::Headless,
            loop_guard: false,
        });
        session
            .create_conversation(
                "age-983".to_string(),
                "Cut off".to_string(),
                &model,
                &provider,
                built.context,
            )
            .await
            .expect("the conversation builds");
        (session, workspace)
    }

    /// Run `input` as one turn of `session`, applying its events.
    async fn run_turn(session: &mut AgentSession, input: TurnInput) -> Vec<SessionEvent> {
        let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
        let sink = events.clone();
        session
            .begin_turn(input, move |event| sink.borrow_mut().push(event))
            .expect("the turn starts")
            .await;
        let events = Rc::try_unwrap(events).unwrap().into_inner();
        for event in &events {
            session.apply(event);
        }
        events
    }

    fn bodies(daemon: &FakeDaemon) -> Vec<String> {
        daemon
            .requests()
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect()
    }

    fn errors(events: &[SessionEvent]) -> Vec<crate::services::StreamError> {
        events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::Error(error) => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    fn tool_errors(events: &[SessionEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::ToolCallError { error, .. } => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    /// The start of a 43-section report as one `write_file` argument, cut
    /// off inside the content string.
    const CUT_OFF_REPORT: &str = "{\"path\": \"report.typ\", \"content\": \"= DABstep report\\n\\n== Q1\\nAnswer: 42\\n\\n== Q2\\nAnsw";

    /// AGE-983, criterion 1: a `write_file` whose arguments are cut off
    /// mid-string does not end the worker. The model gets an error tool
    /// result for it, naming the tool and the column and saying how to
    /// write in parts; it writes the file in two parts with `append`, and
    /// the turn finishes with its answer. The turn's record keeps the
    /// cut-off call and its result.
    #[tokio::test]
    async fn a_cut_off_write_is_answered_and_the_worker_writes_in_parts() {
        let daemon = FakeDaemon::scripted(Script::new().route(
            MODEL,
            [
                Reply::Say("Writing the report.".into()),
                Reply::cut_off_tool_call("write_file", CUT_OFF_REPORT),
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "report.typ", "content": "= DABstep report\n\n== Q1\nAnswer: 42\n\n" }),
                ),
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "report.typ", "content": "== Q2\nAnswer: 7\n", "append": true }),
                ),
                Reply::text("Wrote report.typ in two parts."),
            ],
        ));
        let (mut session, workspace) = worker(&daemon).await;

        let events = run_turn(&mut session, TurnInput::text("Typeset the report.")).await;

        assert!(errors(&events).is_empty(), "{:?}", errors(&events));
        let tool_errors = tool_errors(&events);
        assert_eq!(tool_errors.len(), 1, "{tool_errors:?}");
        assert!(tool_errors[0].starts_with("Error: write_file: this call was cut off"));
        assert!(tool_errors[0].contains("EOF while parsing a string"));
        assert!(tool_errors[0].contains("column"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("report.typ")).unwrap(),
            "= DABstep report\n\n== Q1\nAnswer: 42\n\n== Q2\nAnswer: 7\n"
        );

        let bodies = bodies(&daemon);
        assert_eq!(bodies.len(), 4, "the cut-off call, two parts, the answer");
        assert!(
            bodies[1].contains("this call was cut off")
                && bodies[1].contains("Writing the report."),
            "the next request carries the call, its text and its error result"
        );
        assert!(bodies[3].contains("this call was cut off"), "and keeps it");

        let answer = session
            .conversation()
            .and_then(|conversation| conversation.streaming_message())
            .cloned()
            .unwrap_or_default();
        assert!(
            answer.ends_with("Wrote report.typ in two parts."),
            "{answer}"
        );
        session.finish_turn(None, vec![]);
        let history = serde_json::to_string(&session.conversation().unwrap().messages()).unwrap();
        assert!(
            history.contains("this call was cut off") && history.contains("Typeset the report."),
            "the persisted turn holds the prompt, the cut-off call and the rest"
        );
    }

    /// AGE-983, criterion 2: the recovery is bounded. After
    /// [`MAX_CUT_OFF_RECOVERIES`] answered cut-offs, the next one ends the
    /// turn with rig's error (rig 0.46's wording), as a malformed tool call (the protocol nudge
    /// follows), not a transport failure to re-send.
    #[tokio::test]
    async fn repeated_cut_offs_end_the_turn_after_the_limit() {
        let daemon = FakeDaemon::scripted(Script::new().route(
            MODEL,
            vec![
                Reply::cut_off_tool_call("write_file", CUT_OFF_REPORT);
                MAX_CUT_OFF_RECOVERIES + 2
            ],
        ));
        let (mut session, _workspace) = worker(&daemon).await;

        let events = run_turn(&mut session, TurnInput::text("Typeset the report.")).await;

        assert_eq!(
            bodies(&daemon).len(),
            MAX_CUT_OFF_RECOVERIES + 1,
            "each answered cut-off, then the one that ends the turn"
        );
        assert_eq!(tool_errors(&events).len(), MAX_CUT_OFF_RECOVERIES);
        let errors = errors(&events);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].kind, StreamErrorKind::MalformedToolCall);
        assert!(
            errors[0]
                .message
                .contains("was called with arguments that are not a JSON object"),
            "{}",
            errors[0].message
        );
        let follow_ups: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                SessionEvent::FollowUp(prompt) => Some(prompt.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(follow_ups, vec![MALFORMED_TOOL_CALL_FOLLOW_UP]);
    }
}
