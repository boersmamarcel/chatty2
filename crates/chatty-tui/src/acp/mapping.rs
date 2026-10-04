//! `SessionEvent` → ACP `session/update`: the pure half of `chatty-tui acp`.
//!
//! No I/O and no connection: [`TurnMapper`] turns the events of one turn into
//! the updates an ACP client (Zed, Harbor's ACP runner) renders, and the
//! server in `mod.rs` sends them. Kept apart so the mapping — the interface
//! ADR-0013 says this subcommand shapes — is tested without a transport.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, SessionUpdate,
    TextContent, ToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use chatty_core::services::agent_task_controller::{AgentTaskSnapshot, AgentTodoStatus};
use chatty_core::session::SessionEvent;

use crate::engine::ToolCallState;
use crate::ui::verb;

/// A tool result longer than this is cut before it is sent as the tool
/// call's content: the model already saw the whole of it, and an editor
/// panel has no use for megabytes of `cat` output.
pub(super) const MAX_TOOL_OUTPUT_CHARS: usize = 16_000;

/// The updates of one turn, tracking what a later event needs from an
/// earlier one (a tool's name and input, for its title on completion).
pub(super) struct TurnMapper {
    workspace: PathBuf,
    names: HashMap<String, String>,
    subjects: HashMap<String, String>,
    /// Tool calls started and not yet finished, oldest first: an approval
    /// is raised by the tool that is running, and carries no tool call id.
    running: Vec<String>,
}

impl TurnMapper {
    pub(super) fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            names: HashMap::new(),
            subjects: HashMap::new(),
            running: Vec::new(),
        }
    }

    /// The tool call an approval raised now belongs to: the most recently
    /// started one still running.
    pub(super) fn running_tool_call(&self) -> Option<&str> {
        self.running.last().map(String::as_str)
    }

    /// The updates for `event`; empty for the events that have no ACP
    /// representation or that the server handles itself (approvals,
    /// clarifications, turn lifecycle, usage).
    pub(super) fn map(&mut self, event: &SessionEvent) -> Vec<SessionUpdate> {
        match event {
            SessionEvent::Text(text) if !text.is_empty() => {
                vec![SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text.clone())),
                ))]
            }
            SessionEvent::ToolCallStarted { id, name } => {
                self.names.insert(id.clone(), name.clone());
                self.running.push(id.clone());
                vec![SessionUpdate::ToolCall(
                    ToolCall::new(id.clone(), title(name, "", &ToolCallState::Running))
                        .kind(tool_kind(name))
                        .status(ToolCallStatus::InProgress),
                )]
            }
            SessionEvent::ToolCallInput { id, arguments } => {
                let name = self.names.get(id).cloned().unwrap_or_default();
                let subject = verb::subject_for(arguments);
                self.subjects.insert(id.clone(), subject.clone());
                let raw_input = serde_json::from_str::<serde_json::Value>(arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
                let mut fields = ToolCallUpdateFields::new()
                    .title(title(&name, &subject, &ToolCallState::Running))
                    .raw_input(raw_input.clone());
                if let Some(location) = location(&self.workspace, &raw_input) {
                    fields = fields.locations(vec![location]);
                }
                vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    id.clone(),
                    fields,
                ))]
            }
            SessionEvent::ToolCallResult { id, result } => {
                vec![self.finish(id, result, ToolCallState::Success)]
            }
            SessionEvent::ToolCallError { id, error } => {
                vec![self.finish(id, error, ToolCallState::Error)]
            }
            _ => Vec::new(),
        }
    }

    fn finish(&mut self, id: &str, output: &str, state: ToolCallState) -> SessionUpdate {
        self.running.retain(|running| running != id);
        let status = match state {
            ToolCallState::Error => ToolCallStatus::Failed,
            _ => ToolCallStatus::Completed,
        };
        let content: Vec<ToolCallContent> = if output.is_empty() {
            Vec::new()
        } else {
            vec![ToolCallContent::from(ContentBlock::Text(TextContent::new(
                truncate_output(output),
            )))]
        };
        // A call answered without ever starting — an unknown tool name the
        // repair hook turned away (AGE-497) — is new to the client, so it
        // is announced whole rather than as an update to nothing.
        let Some(name) = self.names.get(id) else {
            return SessionUpdate::ToolCall(
                ToolCall::new(id.to_string(), "Tool call")
                    .status(status)
                    .content(content),
            );
        };
        let subject = self
            .subjects
            .get(id)
            .map(String::as_str)
            .unwrap_or_default();
        let mut fields = ToolCallUpdateFields::new()
            .title(title(name, subject, &state))
            .status(status);
        if !content.is_empty() {
            fields = fields.content(content);
        }
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(id.to_string(), fields))
    }
}

/// The agent's todo list as an ACP plan: the whole list, every time, which
/// is what `session/update` `plan` means.
pub(super) fn plan_update(snapshot: &AgentTaskSnapshot) -> SessionUpdate {
    SessionUpdate::Plan(Plan::new(
        snapshot
            .todos
            .iter()
            .map(|todo| {
                let status = match todo.status {
                    AgentTodoStatus::InProgress => PlanEntryStatus::InProgress,
                    AgentTodoStatus::Done => PlanEntryStatus::Completed,
                    AgentTodoStatus::Pending | AgentTodoStatus::Blocked => PlanEntryStatus::Pending,
                };
                PlanEntry::new(todo.title.clone(), PlanEntryPriority::Medium, status)
            })
            .collect(),
    ))
}

/// The terminal's tool-row wording (`Reading README.md`, `Ran cargo test`),
/// or the tool's own name for a tool that table does not know.
fn title(name: &str, subject: &str, state: &ToolCallState) -> String {
    let head = verb::verb_for(name, state).unwrap_or_else(|| name.to_string());
    if subject.is_empty() {
        head
    } else {
        format!("{head} {subject}")
    }
}

/// What an editor shows the call as. Anything unlisted is `Other`, which a
/// client renders generically.
pub(super) fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read_file"
        | "read_binary"
        | "read_excel"
        | "read_docx"
        | "read_pptx"
        | "read_skill"
        | "list_directory"
        | "file_structure_detector"
        | "pdf_extract_text"
        | "pdf_info"
        | "pdf_to_image"
        | "describe_data"
        | "profile_data"
        | "terminal_read"
        | "git_status"
        | "git_diff"
        | "git_log" => ToolKind::Read,
        "write_file" | "apply_diff" | "create_directory" | "write_excel" | "edit_excel"
        | "write_docx" | "write_pptx" => ToolKind::Edit,
        "delete_file" => ToolKind::Delete,
        "move_file" => ToolKind::Move,
        "glob_search" | "search_code" | "find_files" | "find_definition" | "search_memory"
        | "query_data" => ToolKind::Search,
        "shell_execute" | "execute_code" | "daytona_run" | "git_add" | "git_commit"
        | "git_create_branch" | "git_switch_branch" | "git_merge" => ToolKind::Execute,
        "fetch" | "search_web" | "browser_navigate" => ToolKind::Fetch,
        "write_todos" | "update_todo" | "verify_completion" => ToolKind::Think,
        _ => ToolKind::Other,
    }
}

/// The file a call is about, absolute, so an editor can follow the agent
/// there. Relative paths are the workspace's, as every tool resolves them.
fn location(workspace: &Path, input: &serde_json::Value) -> Option<ToolCallLocation> {
    let path = ["path", "file_path"]
        .iter()
        .find_map(|key| input.get(key).and_then(|v| v.as_str()))
        .filter(|path| !path.is_empty())?;
    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    Some(ToolCallLocation::new(path))
}

fn truncate_output(output: &str) -> String {
    if output.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return output.to_string();
    }
    let kept: String = output.chars().take(MAX_TOOL_OUTPUT_CHARS).collect();
    format!("{kept}\n… (output truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::services::agent_task_controller::AgentTodo;

    fn mapper() -> TurnMapper {
        TurnMapper::new(PathBuf::from("/work"))
    }

    fn json(update: &SessionUpdate) -> serde_json::Value {
        serde_json::to_value(update).unwrap()
    }

    #[test]
    fn text_is_an_agent_message_chunk() {
        let updates = mapper().map(&SessionEvent::Text("hello".into()));
        assert_eq!(updates.len(), 1);
        let v = json(&updates[0]);
        assert_eq!(v["sessionUpdate"], "agent_message_chunk");
        assert_eq!(v["content"]["type"], "text");
        assert_eq!(v["content"]["text"], "hello");
    }

    #[test]
    fn empty_text_sends_nothing() {
        assert!(mapper().map(&SessionEvent::Text(String::new())).is_empty());
    }

    #[test]
    fn a_tool_call_starts_updates_and_completes_under_one_id() {
        let mut m = mapper();
        let started = m.map(&SessionEvent::ToolCallStarted {
            id: "t1".into(),
            name: "read_file".into(),
        });
        let v = json(&started[0]);
        assert_eq!(v["sessionUpdate"], "tool_call");
        assert_eq!(v["toolCallId"], "t1");
        assert_eq!(v["kind"], "read");
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["title"], "Reading");
        assert_eq!(m.running_tool_call(), Some("t1"));

        let input = m.map(&SessionEvent::ToolCallInput {
            id: "t1".into(),
            arguments: r#"{"path":"src/main.rs"}"#.into(),
        });
        let v = json(&input[0]);
        assert_eq!(v["sessionUpdate"], "tool_call_update");
        assert_eq!(v["title"], "Reading src/main.rs");
        assert_eq!(v["rawInput"]["path"], "src/main.rs");
        assert_eq!(v["locations"][0]["path"], "/work/src/main.rs");

        let done = m.map(&SessionEvent::ToolCallResult {
            id: "t1".into(),
            result: "fn main() {}".into(),
        });
        let v = json(&done[0]);
        assert_eq!(v["status"], "completed");
        assert_eq!(v["title"], "Read src/main.rs");
        assert_eq!(v["content"][0]["type"], "content");
        assert_eq!(v["content"][0]["content"]["text"], "fn main() {}");
        assert_eq!(m.running_tool_call(), None);
    }

    #[test]
    fn a_failed_tool_call_is_failed_with_its_error() {
        let mut m = mapper();
        m.map(&SessionEvent::ToolCallStarted {
            id: "t1".into(),
            name: "shell_execute".into(),
        });
        let failed = m.map(&SessionEvent::ToolCallError {
            id: "t1".into(),
            error: "Error: shell_execute: boom".into(),
        });
        let v = json(&failed[0]);
        assert_eq!(v["status"], "failed");
        assert_eq!(v["title"], "Failed Ran");
        assert_eq!(
            v["content"][0]["content"]["text"],
            "Error: shell_execute: boom"
        );
    }

    #[test]
    fn an_unknown_tool_keeps_its_name_as_title() {
        let v = json(
            &mapper().map(&SessionEvent::ToolCallStarted {
                id: "t".into(),
                name: "mcp_thing".into(),
            })[0],
        );
        assert_eq!(v["title"], "mcp_thing");
        assert!(v.get("kind").is_none() || v["kind"] == "other");
    }

    #[test]
    fn the_latest_running_call_owns_an_approval() {
        let mut m = mapper();
        for id in ["a", "b"] {
            m.map(&SessionEvent::ToolCallStarted {
                id: id.into(),
                name: "shell_execute".into(),
            });
        }
        assert_eq!(m.running_tool_call(), Some("b"));
        m.map(&SessionEvent::ToolCallResult {
            id: "b".into(),
            result: "ok".into(),
        });
        assert_eq!(m.running_tool_call(), Some("a"));
    }

    #[test]
    fn a_result_for_a_call_never_started_is_announced_whole() {
        let v = json(
            &mapper().map(&SessionEvent::ToolCallResult {
                id: "x".into(),
                result: "no tool named grep".into(),
            })[0],
        );
        assert_eq!(v["sessionUpdate"], "tool_call");
        assert_eq!(v["toolCallId"], "x");
        assert_eq!(v["title"], "Tool call");
        assert_eq!(v["status"], "completed");
    }

    #[test]
    fn long_output_is_truncated() {
        let long = "x".repeat(MAX_TOOL_OUTPUT_CHARS + 1_000);
        let out = truncate_output(&long);
        assert!(out.ends_with("(output truncated)"));
        assert!(out.chars().count() < long.chars().count());
    }

    #[test]
    fn lifecycle_events_map_to_nothing() {
        let mut m = mapper();
        for event in [
            SessionEvent::TurnStarted,
            SessionEvent::TurnEnded,
            SessionEvent::Cancelled,
            SessionEvent::FollowUp(chatty_core::session::FollowUp::new("next")),
        ] {
            assert!(m.map(&event).is_empty(), "{event:?}");
        }
    }

    #[test]
    fn todos_become_a_plan() {
        let todo = |title: &str, status| AgentTodo {
            id: title.into(),
            title: title.into(),
            description: String::new(),
            status,
            blocked_reason: None,
            reflection: None,
        };
        let snapshot = AgentTaskSnapshot {
            goal: None,
            todos: vec![
                todo("one", AgentTodoStatus::Done),
                todo("two", AgentTodoStatus::InProgress),
                todo("three", AgentTodoStatus::Blocked),
            ],
            write_todos_called: true,
            verified: false,
            verification_reason: None,
            evidence: vec![],
            verification_skipped: false,
        };
        let v = json(&plan_update(&snapshot));
        assert_eq!(v["sessionUpdate"], "plan");
        let statuses: Vec<_> = v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["status"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(statuses, ["completed", "in_progress", "pending"]);
        assert_eq!(v["entries"][0]["content"], "one");
    }
}
