use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::services::{AgentTaskController, AgentTaskResponse, AgentTodoStatus, TodoEntry};
use crate::tools::ToolError;

fn pending() -> AgentTodoStatus {
    AgentTodoStatus::Pending
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TodoInput {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "pending")]
    pub status: AgentTodoStatus,
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WriteTodosArgs {
    #[serde(default)]
    pub goal: Option<String>,
    pub todos: Vec<TodoInput>,
}

/// The agent's todo list: one tool that writes it and updates it, the whole
/// list on every call (AGE-2189, after Claude Code's TodoWrite and Codex's
/// `update_plan`). It replaced `write_todos` + `update_todo`, where a
/// 2-todo plan cost five calls.
#[derive(Clone, Debug)]
pub struct WriteTodosTool {
    controller: AgentTaskController,
}

impl WriteTodosTool {
    pub fn new(controller: AgentTaskController) -> Self {
        Self { controller }
    }
}

impl Tool for WriteTodosTool {
    const NAME: &'static str = "write_todos";
    type Error = ToolError;
    type Args = WriteTodosArgs;
    type Output = AgentTaskResponse;

    fn description(&self) -> String {
        "Write or update the todo list for a task that needs 3 or more distinct tool calls across several files or steps. \
         Every call sends the whole list with each todo's current status and replaces the previous list, so one call can tick off several todos, add a step or drop one. \
         Call it when the size is clear: at the start for a task that is plainly multi-step, or partway through once a task turns out bigger than it looked. \
         Update it when statuses change, not after every tool call. \
         Skip this tool when the task needs fewer than 3 distinct tool calls or can be answered in one response. \
         Never call it for: reading or listing one file, a single edit or rename, or answering a question from information already in the conversation. \
         No plan: 'what does parse_config do?' (read one file, answer); 'fix the typo in the README' (one edit); 'rename foo to bar in two files' (two edits, nothing to track); 'summarise the report above' (already in the conversation). \
         Plan: 'add a dark-mode setting, wire it into the settings page and run the tests' (several files plus a test run); 'fix the 9 failing tests' (one todo per failure, so none is forgotten); a search that found the same change in 8 files (one todo per file). \
         Up to 12 todos. A blocked todo needs a blocked_reason; retry it differently before moving on. \
         A plan of 1 or 2 todos needs no verify_completion once every todo is done."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "One sentence describing the desired end state. Required on the first call; later calls may leave it out."
                },
                "todos": {
                    "type": "array",
                    "description": "The whole ordered list as it stands now, every todo with its current status.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable short id such as t1, kept the same across calls."
                            },
                            "title": {
                                "type": "string",
                                "description": "Short action-oriented title."
                            },
                            "description": {
                                "type": "string",
                                "description": "Optional: the concrete output that makes this todo done."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "done", "blocked"],
                                "description": "Current status; pending when left out."
                            },
                            "blocked_reason": {
                                "type": "string",
                                "description": "Required when status is blocked: what stopped it and what you will try next."
                            }
                        },
                        "required": ["id", "title"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let todos = args
            .todos
            .into_iter()
            .map(|todo| TodoEntry {
                id: todo.id,
                title: todo.title,
                description: todo.description,
                status: todo.status,
                blocked_reason: todo.blocked_reason,
            })
            .collect();
        self.controller
            .write_todos(args.goal, todos)
            .map_err(|error| ToolError::OperationFailed(error.to_string()))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerifyCompletionArgs {
    pub goal_achieved: bool,
    pub reason: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub reflection: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VerifyCompletionTool {
    controller: AgentTaskController,
}

impl VerifyCompletionTool {
    pub fn new(controller: AgentTaskController) -> Self {
        Self { controller }
    }
}

impl Tool for VerifyCompletionTool {
    const NAME: &'static str = "verify_completion";
    type Error = ToolError;
    type Args = VerifyCompletionArgs;
    type Output = AgentTaskResponse;

    fn description(&self) -> String {
        "Check a plan written by write_todos against evidence before writing the final reply: one concrete evidence line per todo. \
         If goal_achieved is false, done todos are reopened: send the list again with write_todos as work continues. Only for a plan of 3 or more todos; skip it otherwise."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "goal_achieved": {
                    "type": "boolean",
                    "description": "True only when the requested end state is achieved."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence explaining the verification result."
                },
                "evidence": {
                    "type": "array",
                    "description": "One concrete evidence line per todo, e.g. 't1: file exists and test passed'.",
                    "items": { "type": "string" }
                },
                "reflection": {
                    "type": "string",
                    "description": "Required when goal_achieved is false; explain what verification revealed."
                }
            },
            "required": ["goal_achieved", "reason", "evidence"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        self.controller
            .verify_completion(
                args.goal_achieved,
                args.reason,
                args.evidence,
                args.reflection,
            )
            .map_err(|error| ToolError::OperationFailed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AGE-479: the description is the gate. It names the threshold and the
    /// cases that must not get a plan, so a model that follows descriptions
    /// loosely still sees them next to the tool itself.
    #[test]
    fn write_todos_description_names_the_negative_cases() {
        let description = WriteTodosTool::new(AgentTaskController::new()).description();
        assert!(description.contains("fewer than 3 distinct tool calls"));
        assert!(description.contains("answered in one response"));
        assert!(description.contains("reading or listing one file"));
        assert!(description.contains("a single edit or rename"));
        assert!(description.contains("information already in the conversation"));
    }

    /// AGE-2188: worked examples on both sides of the threshold, and the
    /// choice may be made partway through a task, not only before it.
    #[test]
    fn write_todos_description_gives_examples_and_allows_a_late_plan() {
        let description = WriteTodosTool::new(AgentTaskController::new()).description();
        assert!(description.contains("No plan: 'what does parse_config do?'"));
        assert!(description.contains("'fix the typo in the README'"));
        assert!(description.contains("Plan: 'add a dark-mode setting"));
        assert!(description.contains("'fix the 9 failing tests'"));
        assert!(description.contains("partway through once a task turns out bigger"));
        assert!(!description.contains("before starting the work"));
        assert!(description.contains("A plan of 1 or 2 todos needs no verify_completion"));
    }

    /// AGE-2189: one tool for the whole list; nothing points at the removed
    /// `update_todo`.
    #[test]
    fn write_todos_description_says_the_whole_list_comes_every_call() {
        let description = WriteTodosTool::new(AgentTaskController::new()).description();
        assert!(description.contains("Every call sends the whole list"));
        assert!(description.contains("one call can tick off several todos"));
        assert!(!description.contains("update_todo"));
    }

    /// The arguments a model sends: status and description may be left out.
    #[test]
    fn a_first_list_needs_only_ids_and_titles() {
        let args: WriteTodosArgs = serde_json::from_value(serde_json::json!({
            "goal": "Ship",
            "todos": [{"id": "t1", "title": "Read"}, {"id": "t2", "title": "Edit", "status": "in_progress"}]
        }))
        .unwrap();
        assert_eq!(args.todos[0].status, AgentTodoStatus::Pending);
        assert_eq!(args.todos[1].status, AgentTodoStatus::InProgress);
        assert!(args.todos[0].description.is_empty());

        let update: WriteTodosArgs = serde_json::from_value(serde_json::json!({
            "todos": [{"id": "t1", "title": "Read", "status": "done"}]
        }))
        .unwrap();
        assert!(update.goal.is_none());
    }

    #[test]
    fn verify_completion_is_for_plans_of_three_or_more() {
        let verify = VerifyCompletionTool::new(AgentTaskController::new()).description();
        assert!(verify.contains("before writing the final reply"));
        assert!(verify.contains("Only for a plan of 3 or more todos; skip it otherwise"));
        assert!(verify.contains("send the list again"));
    }
}
