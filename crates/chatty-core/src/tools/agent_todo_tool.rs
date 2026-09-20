use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};

use crate::services::{AgentTaskController, AgentTaskResponse, AgentTodoStatus};
use crate::tools::ToolError;

#[derive(Debug, Deserialize, Serialize)]
pub struct TodoInput {
    pub id: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WriteTodosArgs {
    pub goal: String,
    pub todos: Vec<TodoInput>,
}

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
        "Write the ordered todo plan for a task that needs 3 or more distinct tool calls across several files or steps, before starting the work. \
         Skip this tool when the task needs fewer than 3 distinct tool calls or can be answered in one response. \
         Never call it for: reading or listing one file, a single edit or rename, or answering a question from information already in the conversation. \
         At most one call per invocation, with a goal and up to 12 concrete todos; revise individual todos with update_todo."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "One sentence describing the desired end state, not the process."
                },
                "todos": {
                    "type": "array",
                    "description": "Flat ordered list of concrete, independently verifiable steps.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable short id such as t1 or inspect-inputs."
                            },
                            "title": {
                                "type": "string",
                                "description": "Short action-oriented title."
                            },
                            "description": {
                                "type": "string",
                                "description": "Concrete verifiable output for this todo."
                            }
                        },
                        "required": ["id", "title", "description"]
                    }
                }
            },
            "required": ["goal", "todos"]
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
            .map(|todo| (todo.id, todo.title, todo.description))
            .collect();
        self.controller
            .write_todos(args.goal, todos)
            .map_err(|error| ToolError::OperationFailed(error.to_string()))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateTodoArgs {
    pub id: String,
    pub status: AgentTodoStatus,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    #[serde(default)]
    pub reflection: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UpdateTodoTool {
    controller: AgentTaskController,
}

impl UpdateTodoTool {
    pub fn new(controller: AgentTaskController) -> Self {
        Self { controller }
    }
}

impl Tool for UpdateTodoTool {
    const NAME: &'static str = "update_todo";
    type Error = ToolError;
    type Args = UpdateTodoArgs;
    type Output = AgentTaskResponse;

    fn description(&self) -> String {
        "Change the status of one todo in the plan written by write_todos. \
         Mark a todo in_progress before working on it and done when finished; mark it blocked, with blocked_reason and reflection, when it cannot be completed, then retry it with a different approach. \
         Only one todo may be in progress at a time. Nothing to do when no plan was written."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Todo id from write_todos."
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "done", "blocked"],
                    "description": "New todo status."
                },
                "blocked_reason": {
                    "type": "string",
                    "description": "Required when status is blocked; explain what prevented completion."
                },
                "reflection": {
                    "type": "string",
                    "description": "Required when status is blocked; explain the wrong assumption and next different approach."
                }
            },
            "required": ["id", "status"]
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
            .update_todo(args.id, args.status, args.blocked_reason, args.reflection)
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
         If goal_achieved is false, done todos are reopened so work continues. Only for tasks with a plan; skip it otherwise."
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

    /// The lifecycle rules live in the descriptions now, not in the preamble:
    /// nothing in them implies a plan is mandatory.
    #[test]
    fn lifecycle_rules_live_in_the_descriptions() {
        let controller = AgentTaskController::new();
        let update = UpdateTodoTool::new(controller.clone()).description();
        assert!(update.contains("in_progress before working on it"));
        assert!(update.contains("blocked_reason and reflection"));
        assert!(update.contains("Nothing to do when no plan was written"));

        let verify = VerifyCompletionTool::new(controller).description();
        assert!(verify.contains("before writing the final reply"));
        assert!(verify.contains("skip it otherwise"));
    }
}
