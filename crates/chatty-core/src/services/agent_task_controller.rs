use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const MAX_TODOS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTodoStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTodo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: AgentTodoStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskSnapshot {
    pub goal: Option<String>,
    pub todos: Vec<AgentTodo>,
    pub write_todos_called: bool,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_reason: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskResponse {
    pub message: String,
    pub snapshot: AgentTaskSnapshot,
    pub should_ask_user: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentTaskError {
    #[error(
        "write_todos has already been called for this agent invocation; use update_todo to revise individual todos"
    )]
    WriteTodosAlreadyCalled,
    #[error("goal must not be empty")]
    EmptyGoal,
    #[error("todos must contain between 1 and {MAX_TODOS} items")]
    InvalidTodoCount,
    #[error("todo id, title, and description must not be empty")]
    EmptyTodoField,
    #[error("duplicate todo id: {0}")]
    DuplicateTodoId(String),
    #[error("write_todos must be called before update_todo or verify_completion")]
    MissingTodos,
    #[error("unknown todo id: {0}")]
    UnknownTodo(String),
    #[error("todo {0} is already in progress; finish or block it before starting another todo")]
    TodoAlreadyInProgress(String),
    #[error("blocked todos require blocked_reason and reflection")]
    MissingBlockedDetails,
    #[error("verification requires at least one evidence line")]
    MissingEvidence,
    #[error("all todos must be done before verify_completion can succeed")]
    TodosNotDone,
}

#[derive(Debug, Default)]
struct AgentTaskState {
    goal: Option<String>,
    todos: Vec<AgentTodo>,
    write_todos_called: bool,
    verified: bool,
    verification_reason: Option<String>,
    evidence: Vec<String>,
    blocked_counts: HashMap<String, usize>,
    non_todo_tool_results_without_plan: usize,
}

#[derive(Clone, Debug, Default)]
pub struct AgentTaskController {
    state: Arc<Mutex<AgentTaskState>>,
}

impl AgentTaskController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write_todos(
        &self,
        goal: String,
        todos: Vec<(String, String, String)>,
    ) -> Result<AgentTaskResponse, AgentTaskError> {
        let goal = goal.trim().to_string();
        if goal.is_empty() {
            return Err(AgentTaskError::EmptyGoal);
        }
        if todos.is_empty() || todos.len() > MAX_TODOS {
            return Err(AgentTaskError::InvalidTodoCount);
        }

        let mut ids = HashSet::new();
        let mut normalized = Vec::with_capacity(todos.len());
        for (id, title, description) in todos {
            let id = id.trim().to_string();
            let title = title.trim().to_string();
            let description = description.trim().to_string();
            if id.is_empty() || title.is_empty() || description.is_empty() {
                return Err(AgentTaskError::EmptyTodoField);
            }
            if !ids.insert(id.clone()) {
                return Err(AgentTaskError::DuplicateTodoId(id));
            }
            normalized.push(AgentTodo {
                id,
                title,
                description,
                status: AgentTodoStatus::Pending,
                blocked_reason: None,
                reflection: None,
            });
        }

        let mut state = self.state.lock();
        if state.write_todos_called {
            return Err(AgentTaskError::WriteTodosAlreadyCalled);
        }

        state.goal = Some(goal);
        state.todos = normalized;
        state.write_todos_called = true;
        state.verified = false;
        state.verification_reason = None;
        state.evidence.clear();
        state.blocked_counts.clear();
        state.non_todo_tool_results_without_plan = 0;

        Ok(response(
            "Todo plan recorded. Start the first pending todo with update_todo(status=\"in_progress\").",
            &state,
            false,
        ))
    }

    pub fn update_todo(
        &self,
        id: String,
        status: AgentTodoStatus,
        blocked_reason: Option<String>,
        reflection: Option<String>,
    ) -> Result<AgentTaskResponse, AgentTaskError> {
        let mut state = self.state.lock();
        if !state.write_todos_called {
            return Err(AgentTaskError::MissingTodos);
        }

        if matches!(status, AgentTodoStatus::InProgress)
            && let Some(active) = state
                .todos
                .iter()
                .find(|todo| todo.status == AgentTodoStatus::InProgress && todo.id != id)
        {
            return Err(AgentTaskError::TodoAlreadyInProgress(active.id.clone()));
        }

        let todo = state
            .todos
            .iter_mut()
            .find(|todo| todo.id == id)
            .ok_or_else(|| AgentTaskError::UnknownTodo(id.clone()))?;

        if matches!(status, AgentTodoStatus::Blocked)
            && (blocked_reason
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
                || reflection.as_deref().unwrap_or_default().trim().is_empty())
        {
            return Err(AgentTaskError::MissingBlockedDetails);
        }

        todo.status = status;
        todo.blocked_reason = blocked_reason.filter(|value| !value.trim().is_empty());
        todo.reflection = reflection.filter(|value| !value.trim().is_empty());
        state.verified = false;
        state.verification_reason = None;
        state.evidence.clear();

        let mut should_ask_user = false;
        if matches!(status, AgentTodoStatus::Blocked) {
            let count = state.blocked_counts.entry(id).or_insert(0);
            *count += 1;
            should_ask_user = *count >= 2;
        }

        let message = match status {
            AgentTodoStatus::Pending => "Todo reopened as pending.",
            AgentTodoStatus::InProgress => {
                "Todo marked in progress. Do the work for this todo only."
            }
            AgentTodoStatus::Done => {
                "Todo marked done. Start the next pending todo or verify completion if all todos are done."
            }
            AgentTodoStatus::Blocked if should_ask_user => {
                "Todo blocked again. Ask the user one targeted question before continuing."
            }
            AgentTodoStatus::Blocked => {
                "Todo marked blocked. Retry this todo with a different approach before moving on."
            }
        };

        Ok(response(message, &state, should_ask_user))
    }

    pub fn verify_completion(
        &self,
        goal_achieved: bool,
        reason: String,
        evidence: Vec<String>,
        reflection: Option<String>,
    ) -> Result<AgentTaskResponse, AgentTaskError> {
        let mut state = self.state.lock();
        if !state.write_todos_called {
            return Err(AgentTaskError::MissingTodos);
        }
        let evidence: Vec<String> = evidence
            .into_iter()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        if evidence.is_empty() {
            return Err(AgentTaskError::MissingEvidence);
        }
        if goal_achieved
            && state
                .todos
                .iter()
                .any(|todo| todo.status != AgentTodoStatus::Done)
        {
            return Err(AgentTaskError::TodosNotDone);
        }

        state.verified = goal_achieved;
        state.verification_reason = Some(reason.trim().to_string());
        state.evidence = evidence;

        if !goal_achieved {
            for todo in &mut state.todos {
                if todo.status == AgentTodoStatus::Done {
                    todo.status = AgentTodoStatus::Pending;
                }
                if let Some(reflection) = reflection.as_ref() {
                    todo.reflection = Some(reflection.clone());
                }
            }
        }

        let message = if goal_achieved {
            "Verification accepted. You may now write the final reply to the user."
        } else {
            "Verification failed. Reopened completed todos; continue from the first pending todo."
        };

        Ok(response(message, &state, false))
    }

    pub fn snapshot(&self) -> AgentTaskSnapshot {
        snapshot(&self.state.lock())
    }

    pub fn observe_tool_result(&self, tool_name: &str) -> Option<String> {
        if matches!(
            tool_name,
            "write_todos" | "update_todo" | "verify_completion"
        ) {
            return None;
        }

        let mut state = self.state.lock();
        if state.write_todos_called {
            return None;
        }

        state.non_todo_tool_results_without_plan += 1;
        if state.non_todo_tool_results_without_plan >= 2 {
            tracing::warn!(
                tool_name = %tool_name,
                tool_results = state.non_todo_tool_results_without_plan,
                "Agent todo protocol violation: multiple non-todo tool results before write_todos"
            );
            Some(
                "This has become a multi-step task. Call write_todos now with a concrete ordered plan before continuing with more work."
                    .to_string(),
            )
        } else {
            None
        }
    }

    pub fn stream_end_follow_up(&self) -> Option<String> {
        let state = self.state.lock();
        if state.write_todos_called && !state.verified {
            tracing::warn!("Agent todo protocol violation: stream ended before verify_completion");
            Some(
                "Before writing the final reply, call verify_completion with concrete evidence for each todo. If verification fails, continue from the reopened todo instead of responding to the user."
                    .to_string(),
            )
        } else if state
            .todos
            .iter()
            .any(|todo| todo.status == AgentTodoStatus::InProgress)
        {
            tracing::warn!("Agent todo protocol violation: stream ended with in-progress todo");
            Some(
                "A todo is still in progress. Call update_todo with status=\"done\" or status=\"blocked\" before moving on or replying."
                    .to_string(),
            )
        } else {
            None
        }
    }
}

fn response(message: &str, state: &AgentTaskState, should_ask_user: bool) -> AgentTaskResponse {
    AgentTaskResponse {
        message: message.to_string(),
        snapshot: snapshot(state),
        should_ask_user,
    }
}

fn snapshot(state: &AgentTaskState) -> AgentTaskSnapshot {
    AgentTaskSnapshot {
        goal: state.goal.clone(),
        todos: state.todos.clone(),
        write_todos_called: state.write_todos_called,
        verified: state.verified,
        verification_reason: state.verification_reason.clone(),
        evidence: state.evidence.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> AgentTaskController {
        AgentTaskController::new()
    }

    fn todos() -> Vec<(String, String, String)> {
        vec![
            ("t1".into(), "Plan".into(), "Create a plan.".into()),
            (
                "t2".into(),
                "Implement".into(),
                "Implement the change.".into(),
            ),
        ]
    }

    #[test]
    fn write_todos_records_initial_state() {
        let controller = controller();
        let response = controller
            .write_todos("Ship the feature".into(), todos())
            .unwrap();

        assert!(response.snapshot.write_todos_called);
        assert_eq!(response.snapshot.todos.len(), 2);
        assert_eq!(response.snapshot.todos[0].status, AgentTodoStatus::Pending);
    }

    #[test]
    fn write_todos_is_single_use() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();
        let error = controller
            .write_todos("Ship again".into(), todos())
            .unwrap_err();

        assert!(matches!(error, AgentTaskError::WriteTodosAlreadyCalled));
    }

    #[test]
    fn only_one_todo_can_be_in_progress() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();
        controller
            .update_todo("t1".into(), AgentTodoStatus::InProgress, None, None)
            .unwrap();

        let error = controller
            .update_todo("t2".into(), AgentTodoStatus::InProgress, None, None)
            .unwrap_err();
        assert!(matches!(error, AgentTaskError::TodoAlreadyInProgress(_)));
    }

    #[test]
    fn blocked_twice_requests_user_question() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();

        let first = controller
            .update_todo(
                "t1".into(),
                AgentTodoStatus::Blocked,
                Some("tool failed".into()),
                Some("try another approach".into()),
            )
            .unwrap();
        let second = controller
            .update_todo(
                "t1".into(),
                AgentTodoStatus::Blocked,
                Some("alternate failed".into()),
                Some("need clarification".into()),
            )
            .unwrap();

        assert!(!first.should_ask_user);
        assert!(second.should_ask_user);
    }

    #[test]
    fn verify_requires_all_todos_done_for_success() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();

        let error = controller
            .verify_completion(true, "not yet".into(), vec!["t1: pending".into()], None)
            .unwrap_err();
        assert!(matches!(error, AgentTaskError::TodosNotDone));
    }

    #[test]
    fn verify_false_reopens_done_todos() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();
        controller
            .update_todo("t1".into(), AgentTodoStatus::Done, None, None)
            .unwrap();

        let response = controller
            .verify_completion(
                false,
                "missing implementation".into(),
                vec!["t1: plan exists".into()],
                Some("verification showed the implementation is missing".into()),
            )
            .unwrap();

        assert!(!response.snapshot.verified);
        assert_eq!(response.snapshot.todos[0].status, AgentTodoStatus::Pending);
    }

    #[test]
    fn observe_tool_result_prompts_after_repeated_work_without_plan() {
        let controller = controller();

        assert!(controller.observe_tool_result("read_file").is_none());
        let prompt = controller.observe_tool_result("search_code");

        assert!(prompt.is_some());
        assert!(prompt.unwrap().contains("write_todos"));
    }

    #[test]
    fn stream_end_requires_verification_after_todos() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();
        controller
            .update_todo("t1".into(), AgentTodoStatus::Done, None, None)
            .unwrap();
        controller
            .update_todo("t2".into(), AgentTodoStatus::Done, None, None)
            .unwrap();

        let prompt = controller.stream_end_follow_up();

        assert!(prompt.is_some());
        assert!(prompt.unwrap().contains("verify_completion"));
    }

    #[test]
    fn stream_end_allows_verified_completion() {
        let controller = controller();
        controller.write_todos("Ship".into(), todos()).unwrap();
        controller
            .update_todo("t1".into(), AgentTodoStatus::Done, None, None)
            .unwrap();
        controller
            .update_todo("t2".into(), AgentTodoStatus::Done, None, None)
            .unwrap();
        controller
            .verify_completion(
                true,
                "all done".into(),
                vec!["t1: done".into(), "t2: done".into()],
                None,
            )
            .unwrap();

        assert!(controller.stream_end_follow_up().is_none());
    }
}
