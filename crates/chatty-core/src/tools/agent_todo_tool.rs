use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
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

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Create the single ordered todo plan for a multi-step task before doing any work. Call this once per agent invocation with a goal and up to 12 concrete todos.".to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Update exactly one todo before and after working on it. Mark it in_progress before work, then done or blocked with reason and reflection.".to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Verify every todo has real evidence before writing the final reply. If goal_achieved is false, completed todos are reopened so work can continue.".to_string(),
            parameters: serde_json::json!({
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
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
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
