use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const MAX_TODOS: usize = 12;

/// Maximum number of protocol follow-ups the harness injects for a single user
/// turn. Mirrors `AgentLoopGuard::MAX_LOOP_PIVOTS`: without a cap, a model that
/// answers every nudge with prose keeps the harness re-injecting forever
/// (AGE-150). `reset()` restores the budget when a new user turn starts.
const MAX_PROTOCOL_FOLLOW_UPS: usize = 3;

/// A plan with fewer todos than this ends without `verify_completion`
/// once every todo is done (AGE-2188). A one- or two-step plan is cheap
/// bookkeeping; asking for an evidence round on top made the model pay
/// five or more extra tool calls for a small task.
const VERIFY_FROM_TODOS: usize = 3;

/// How many times one user turn's `verify_completion` refuses a coordinator
/// that has not delegated yet (AGE-2179). After that the model's own
/// judgement stands: a task its team has no part in still finishes.
const MAX_DELEGATION_NUDGES: usize = 2;

/// What a coordinator that has not delegated is told (AGE-2179), by
/// `verify_completion` and at the end of a turn.
const DELEGATION_NUDGE: &str = "you have not delegated any of this work. You coordinate: call invoke_agent so your team does it,";

/// Tool calls after which the agent hears, once, that the todo list exists
/// (no plan yet) or has gone stale (a plan with open todos that no
/// `write_todos` call has touched for this many calls). AGE-2189: the same
/// gentle nudge Claude Code gives, so a task that grows midway can still
/// get a plan without one being forced up front.
pub const REMIND_AFTER_TOOL_CALLS: usize = 8;

/// The note on a tool result once a turn without a plan reaches
/// [`REMIND_AFTER_TOOL_CALLS`] tool calls.
pub const NO_PLAN_REMINDER: &str = "[Reminder: this task has taken several tool calls. \
If several steps are still left, write_todos can track them; skip it if the task is nearly done. \
Do not mention this reminder to the user.]";

/// The note on a tool result once a plan with open todos has gone
/// [`REMIND_AFTER_TOOL_CALLS`] tool calls without a `write_todos` call.
pub const STALE_PLAN_REMINDER: &str = "[Reminder: the todo list has not been updated for several tool calls. \
If todos are finished, send the list again with write_todos and their new statuses; ignore this if the list is current. \
Do not mention this reminder to the user.]";

/// The rig hook that puts the controller's reminder ([`NO_PLAN_REMINDER`],
/// [`STALE_PLAN_REMINDER`]) on the tool result that reaches the threshold
/// (AGE-2189). Like the turn budget's notes, it rides inside the tool
/// result, so history pairing and the transcript are unaffected and no user
/// message is invented.
#[derive(Clone, Debug)]
pub struct TodoReminder(pub AgentTaskController);

impl rig_agent::agent::AgentHook for TodoReminder {
    async fn on_outcome(
        &self,
        _ctx: &rig_agent::agent::HookContext,
        event: rig_agent::agent::OutcomeEvent<'_>,
    ) -> rig_agent::agent::OutcomeAction {
        let (Some(result), Some(name)) = (event.tool_result(), event.tool_name()) else {
            return rig_agent::agent::OutcomeAction::Proceed;
        };
        match self.0.note_tool_call(name) {
            Some(note) => rig_agent::agent::OutcomeAction::rewrite_tool_output(
                &event,
                crate::services::turn_budget::with_note(result.output(), note),
            ),
            None => rig_agent::agent::OutcomeAction::Proceed,
        }
    }
}

/// One todo as `write_todos` receives it: the whole list comes on every
/// call, each todo with its current status.
#[derive(Debug, Clone)]
pub struct TodoEntry {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: AgentTodoStatus,
    pub blocked_reason: Option<String>,
}

impl TodoEntry {
    /// A pending todo, the shape of a first plan.
    pub fn pending(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            status: AgentTodoStatus::Pending,
            blocked_reason: None,
        }
    }

    /// The same todo with `status`.
    pub fn with_status(mut self, status: AgentTodoStatus) -> Self {
        self.status = status;
        self
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskSnapshot {
    pub goal: Option<String>,
    pub todos: Vec<AgentTodo>,
    pub write_todos_called: bool,
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_reason: Option<String>,
    pub evidence: Vec<String>,
    /// True once the follow-up budget ran out with verification still pending,
    /// so the UI can show that verification was skipped rather than silently
    /// leaving the plan unfinished.
    #[serde(default)]
    pub verification_skipped: bool,
}

impl AgentTaskSnapshot {
    /// `(done, total, blocked, current step)`: the one reading of a plan's
    /// progress both frontends show. `total` is at least 1; the current step
    /// is the first in-progress todo, else the first pending one, else
    /// `"{done}/{total} complete"`.
    pub fn counts(&self) -> (usize, usize, usize, String) {
        let count = |status: AgentTodoStatus| {
            self.todos
                .iter()
                .filter(|todo| todo.status == status)
                .count()
        };
        let first = |status: AgentTodoStatus| {
            self.todos
                .iter()
                .find(|todo| todo.status == status)
                .map(|todo| todo.title.clone())
        };
        let done = count(AgentTodoStatus::Done);
        let blocked = count(AgentTodoStatus::Blocked);
        let total = self.todos.len().max(1);
        let current = first(AgentTodoStatus::InProgress)
            .or_else(|| first(AgentTodoStatus::Pending))
            .unwrap_or_else(|| format!("{done}/{total} complete"));
        (done, total, blocked, current)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskResponse {
    pub message: String,
    pub snapshot: AgentTaskSnapshot,
    pub should_ask_user: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentTaskError {
    #[error("goal must not be empty on the first write_todos call")]
    EmptyGoal,
    #[error("todos must contain between 1 and {MAX_TODOS} items")]
    InvalidTodoCount,
    #[error("todo id and title must not be empty")]
    EmptyTodoField,
    #[error("duplicate todo id: {0}")]
    DuplicateTodoId(String),
    #[error("write_todos must be called before verify_completion")]
    MissingTodos,
    #[error("blocked todo {0} needs a blocked_reason: what stopped it and what you will try next")]
    MissingBlockedReason(String),
    #[error("verification requires at least one evidence line")]
    MissingEvidence,
    #[error("all todos must be done before verify_completion can succeed")]
    TodosNotDone,
    /// A coordinator that read and planned but started no worker (AGE-2179).
    #[error("not verified: {DELEGATION_NUDGE} then verify again.")]
    NotDelegated,
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
    follow_up_count: usize,
    verification_skipped: bool,
    /// Whether this agent is offered `write_todos` at all; a worker profile
    /// without it never hears a reminder. Kept across [`reset`].
    ///
    /// [`reset`]: AgentTaskController::reset
    offers_plan: bool,
    /// Non-todo tool calls this user turn while no plan exists.
    calls_without_plan: usize,
    /// Non-todo tool calls since the last `write_todos`.
    calls_since_write: usize,
    no_plan_reminded: bool,
    stale_plan_reminded: bool,
    /// Whether this turn called `invoke_agent` (AGE-2179).
    delegated: bool,
    delegation_nudges: usize,
}

#[derive(Clone, Debug, Default)]
pub struct AgentTaskController {
    state: Arc<Mutex<AgentTaskState>>,
    /// A coordinator that delegates: its `verify_completion` passes only
    /// once it has delegated (AGE-2179). Fixed at build, kept by `reset`.
    requires_delegation: bool,
}

impl AgentTaskController {
    pub fn new() -> Self {
        Self::default()
    }

    /// A controller whose `verify_completion` refuses success until the
    /// agent has delegated this turn ([`Self::note_delegation`]), for a
    /// coordinator, whose work is done by its team (AGE-2179).
    pub fn requiring_delegation(mut self, required: bool) -> Self {
        self.requires_delegation = required;
        self
    }

    /// The agent called `invoke_agent` this turn.
    pub fn note_delegation(&self) {
        self.state.lock().delegated = true;
    }

    /// Clear all task state so a new user turn starts from scratch.
    ///
    /// The controller lives inside the conversation's `AgentClient` and
    /// therefore outlives a single send. Without this, an unverified plan makes
    /// every later message in the conversation end with the same nudge and
    /// blocks `write_todos` for good (AGE-150).
    pub fn reset(&self) {
        let mut state = self.state.lock();
        let offers_plan = state.offers_plan;
        *state = AgentTaskState {
            offers_plan,
            ..AgentTaskState::default()
        };
    }

    /// Record whether this agent is offered `write_todos`. Only then does
    /// [`note_tool_call`](Self::note_tool_call) ever return a reminder.
    pub fn set_offers_plan(&self, offers: bool) {
        self.state.lock().offers_plan = offers;
    }

    /// Count one finished tool call named `tool_name` and return the
    /// reminder its result should carry, if any (AGE-2189).
    ///
    /// Todo tools are not counted. A turn without a plan hears
    /// [`NO_PLAN_REMINDER`] once, at [`REMIND_AFTER_TOOL_CALLS`] calls. A
    /// plan with open todos hears [`STALE_PLAN_REMINDER`] once per stretch
    /// of that many calls without a `write_todos`; the next write re-arms it.
    pub fn note_tool_call(&self, tool_name: &str) -> Option<&'static str> {
        if is_agent_todo_tool(tool_name) {
            return None;
        }
        let mut state = self.state.lock();
        if !state.offers_plan {
            return None;
        }
        if !state.write_todos_called {
            state.calls_without_plan += 1;
            if !state.no_plan_reminded && state.calls_without_plan >= REMIND_AFTER_TOOL_CALLS {
                state.no_plan_reminded = true;
                return Some(NO_PLAN_REMINDER);
            }
            return None;
        }
        state.calls_since_write += 1;
        let open = state.todos.iter().any(|todo| {
            matches!(
                todo.status,
                AgentTodoStatus::Pending | AgentTodoStatus::InProgress
            )
        });
        if open && !state.stale_plan_reminded && state.calls_since_write >= REMIND_AFTER_TOOL_CALLS
        {
            state.stale_plan_reminded = true;
            return Some(STALE_PLAN_REMINDER);
        }
        None
    }

    /// Replace the plan with `todos`, the whole list as it stands now
    /// (AGE-2189: one tool for writing and updating, as in Claude Code's
    /// TodoWrite). `goal` is needed on the first call only; a later call
    /// without one keeps the plan's goal.
    ///
    /// Any change resets verification. A todo that becomes blocked counts
    /// against its id; the second time, the response asks the agent to put
    /// one question to the user.
    pub fn write_todos(
        &self,
        goal: Option<String>,
        todos: Vec<TodoEntry>,
    ) -> Result<AgentTaskResponse, AgentTaskError> {
        if todos.is_empty() || todos.len() > MAX_TODOS {
            return Err(AgentTaskError::InvalidTodoCount);
        }

        let mut ids = HashSet::new();
        let mut normalized = Vec::with_capacity(todos.len());
        for todo in todos {
            let id = todo.id.trim().to_string();
            let title = todo.title.trim().to_string();
            if id.is_empty() || title.is_empty() {
                return Err(AgentTaskError::EmptyTodoField);
            }
            if !ids.insert(id.clone()) {
                return Err(AgentTaskError::DuplicateTodoId(id));
            }
            let blocked_reason = todo
                .blocked_reason
                .map(|reason| reason.trim().to_string())
                .filter(|reason| !reason.is_empty());
            if todo.status == AgentTodoStatus::Blocked && blocked_reason.is_none() {
                return Err(AgentTaskError::MissingBlockedReason(id));
            }
            normalized.push(AgentTodo {
                id,
                title,
                description: todo.description.trim().to_string(),
                status: todo.status,
                blocked_reason: blocked_reason.filter(|_| todo.status == AgentTodoStatus::Blocked),
                reflection: None,
            });
        }

        let mut state = self.state.lock();
        let goal = goal
            .map(|goal| goal.trim().to_string())
            .filter(|goal| !goal.is_empty())
            .or_else(|| state.goal.clone());
        let Some(goal) = goal else {
            return Err(AgentTaskError::EmptyGoal);
        };

        let mut should_ask_user = false;
        for todo in &normalized {
            if todo.status != AgentTodoStatus::Blocked {
                continue;
            }
            let was_blocked = state
                .todos
                .iter()
                .any(|old| old.id == todo.id && old.status == AgentTodoStatus::Blocked);
            if !was_blocked {
                let count = state.blocked_counts.entry(todo.id.clone()).or_insert(0);
                *count += 1;
                should_ask_user |= *count >= 2;
            }
        }

        let first = !state.write_todos_called;
        state.goal = Some(goal);
        state.todos = normalized;
        state.write_todos_called = true;
        state.verified = false;
        state.verification_reason = None;
        state.evidence.clear();
        state.verification_skipped = false;
        state.calls_since_write = 0;
        state.stale_plan_reminded = false;

        let all_done = state
            .todos
            .iter()
            .all(|todo| todo.status == AgentTodoStatus::Done);
        let message = if should_ask_user {
            "Todo list updated. A todo is blocked a second time: ask the user one targeted question before continuing."
        } else if state
            .todos
            .iter()
            .any(|todo| todo.status == AgentTodoStatus::Blocked)
        {
            "Todo list updated. Retry the blocked todo with a different approach before moving on."
        } else if all_done && state.todos.len() >= VERIFY_FROM_TODOS {
            "Todo list updated. All todos are done: call verify_completion with evidence before the final reply."
        } else if all_done {
            "Todo list updated. All todos are done."
        } else if first {
            "Todo list recorded. Work through it, and send the whole list again with write_todos when statuses change."
        } else {
            "Todo list updated."
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
        if goal_achieved
            && self.requires_delegation
            && !state.delegated
            && state.delegation_nudges < MAX_DELEGATION_NUDGES
        {
            state.delegation_nudges += 1;
            return Err(AgentTaskError::NotDelegated);
        }

        state.verified = goal_achieved;
        state.verification_skipped = false;
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

    /// The nudge to inject after a stream ends, or `None` when the agent is
    /// done or the follow-up budget is spent.
    ///
    /// A turn that never wrote a plan gets nothing: whether a task needs one
    /// is the model's call, made from the `write_todos` description
    /// (AGE-479). The protocol only steers a plan that exists.
    ///
    /// A small plan (fewer than [`VERIFY_FROM_TODOS`] todos) whose todos are
    /// all done needs no verification round either (AGE-2188).
    ///
    /// The in-progress check comes first on purpose: an in-progress todo makes
    /// `verify_completion` fail with `TodosNotDone`, so asking for verification
    /// there is an instruction the state machine forbids (AGE-150).
    pub fn stream_end_follow_up(&self) -> Option<String> {
        let mut state = self.state.lock();
        if state.verified {
            return None;
        }
        // A coordinator that started no worker is told to, plan or not: a
        // small plan, or none, would otherwise end here unasked (AGE-2179).
        if self.requires_delegation
            && !state.delegated
            && state.delegation_nudges < MAX_DELEGATION_NUDGES
        {
            state.delegation_nudges += 1;
            tracing::warn!("Agent protocol: coordinator turn ended without a delegation");
            return Some(format!(
                "Agent protocol follow-up: {DELEGATION_NUDGE} then reply."
            ));
        }
        if !state.write_todos_called {
            return None;
        }

        if state.todos.len() < VERIFY_FROM_TODOS
            && state
                .todos
                .iter()
                .all(|todo| todo.status == AgentTodoStatus::Done)
        {
            return None;
        }

        if state.follow_up_count >= MAX_PROTOCOL_FOLLOW_UPS {
            tracing::warn!(
                follow_ups = state.follow_up_count,
                "Agent todo protocol: follow-up budget exhausted; leaving the reply unverified"
            );
            state.verification_skipped = true;
            return None;
        }
        state.follow_up_count += 1;

        if state
            .todos
            .iter()
            .any(|todo| todo.status == AgentTodoStatus::InProgress)
        {
            tracing::warn!("Agent todo protocol violation: stream ended with in-progress todo");
            Some(
                "A todo is still in progress. Send the list again with write_todos, each todo marked done or blocked, before replying."
                    .to_string(),
            )
        } else {
            tracing::warn!("Agent todo protocol violation: stream ended before verify_completion");
            Some(
                "Before writing the final reply, call verify_completion with concrete evidence for each todo. If verification fails, continue from the reopened todo instead of responding to the user."
                    .to_string(),
            )
        }
    }
}

/// True for injected agent-protocol / loop-guard follow-ups that must reach the
/// LLM but must not appear as a user/system bubble in the transcript.
pub fn is_protocol_follow_up_text(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("Before writing the final reply, call verify_completion")
        // Both the current nudge and the `update_todo` one that older
        // transcripts hold (AGE-2189).
        || t.starts_with("A todo is still in progress.")
        || t.starts_with("LOOP DETECTED:")
        || t.starts_with("DEADLINE:")
        || t.starts_with("Agent protocol follow-up:")
        // The loop guard's nudge after a long text-only answer (AGE-2129):
        // without the prefix, it showed as the person's own message.
        || t.starts_with(crate::session::BREVITY_FOLLOW_UP)
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
        verification_skipped: state.verification_skipped,
    }
}

/// Whether `name` is one of the tools that own the agent's plan.
///
/// These render as the plan card, not as individual tool calls (AGE-342).
/// `update_todo` is no longer offered (AGE-2189) but stays here so the
/// transcripts that hold its calls still render them as the plan.
pub fn is_agent_todo_tool(name: &str) -> bool {
    matches!(name, "write_todos" | "update_todo" | "verify_completion")
}

/// The plan snapshot a todo tool returned alongside its model-directed
/// `message`, so a frontend can render the plan's state without threading the
/// controller through to the UI.
pub fn snapshot_from_tool_output(output: &str) -> Option<AgentTaskSnapshot> {
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    serde_json::from_value(value.get("snapshot")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentTodoStatus::{Blocked, Done, InProgress, Pending};

    fn controller() -> AgentTaskController {
        AgentTaskController::new()
    }

    /// A plan of `statuses.len()` todos `t1`, `t2`, … with these statuses.
    fn list(statuses: &[AgentTodoStatus]) -> Vec<TodoEntry> {
        statuses
            .iter()
            .enumerate()
            .map(|(n, status)| {
                let id = format!("t{}", n + 1);
                TodoEntry::pending(id.clone(), format!("Step {id}"), "").with_status(*status)
            })
            .collect()
    }

    fn write(controller: &AgentTaskController, statuses: &[AgentTodoStatus]) -> AgentTaskResponse {
        controller
            .write_todos(Some("Ship".into()), list(statuses))
            .unwrap()
    }

    fn blocked(id: &str, reason: &str) -> TodoEntry {
        TodoEntry {
            blocked_reason: Some(reason.into()),
            ..TodoEntry::pending(id, "Step", "").with_status(Blocked)
        }
    }

    #[test]
    fn write_todos_records_the_list_as_sent() {
        let response = write(&controller(), &[InProgress, Pending]);

        assert!(response.snapshot.write_todos_called);
        assert_eq!(response.snapshot.goal.as_deref(), Some("Ship"));
        assert_eq!(response.snapshot.todos.len(), 2);
        assert_eq!(response.snapshot.todos[0].status, InProgress);
        assert_eq!(response.snapshot.todos[1].status, Pending);
    }

    /// AGE-2189: the whole list comes on every call, so ticking off two
    /// todos is one call and a 2-todo plan costs two calls in all.
    #[test]
    fn a_second_call_replaces_the_list_and_keeps_the_goal() {
        let controller = controller();
        write(&controller, &[Pending, Pending]);

        let response = controller.write_todos(None, list(&[Done, Done])).unwrap();

        assert_eq!(response.snapshot.goal.as_deref(), Some("Ship"));
        assert!(
            response
                .snapshot
                .todos
                .iter()
                .all(|todo| todo.status == Done)
        );
        assert!(controller.stream_end_follow_up().is_none());
    }

    #[test]
    fn a_later_call_may_add_and_drop_todos() {
        let controller = controller();
        write(&controller, &[Done, Pending]);
        let response = write(&controller, &[Done, Done, Pending]);
        assert_eq!(response.snapshot.todos.len(), 3);
    }

    #[test]
    fn the_first_call_needs_a_goal() {
        let error = controller()
            .write_todos(Some("  ".into()), list(&[Pending]))
            .unwrap_err();
        assert!(matches!(error, AgentTaskError::EmptyGoal));
    }

    #[test]
    fn several_todos_may_be_in_progress() {
        let response = write(&controller(), &[InProgress, InProgress]);
        assert_eq!(response.snapshot.todos[1].status, InProgress);
    }

    #[test]
    fn the_list_is_checked() {
        let controller = controller();
        assert!(matches!(
            controller.write_todos(Some("Ship".into()), Vec::new()),
            Err(AgentTaskError::InvalidTodoCount)
        ));
        let mut twice = list(&[Pending, Pending]);
        twice[1].id = "t1".into();
        assert!(matches!(
            controller.write_todos(Some("Ship".into()), twice),
            Err(AgentTaskError::DuplicateTodoId(id)) if id == "t1"
        ));
        let mut untitled = list(&[Pending]);
        untitled[0].title = " ".into();
        assert!(matches!(
            controller.write_todos(Some("Ship".into()), untitled),
            Err(AgentTaskError::EmptyTodoField)
        ));
    }

    #[test]
    fn a_blocked_todo_needs_a_reason() {
        let error = write_err(&[Blocked]);
        assert!(matches!(error, AgentTaskError::MissingBlockedReason(id) if id == "t1"));
    }

    fn write_err(statuses: &[AgentTodoStatus]) -> AgentTaskError {
        controller()
            .write_todos(Some("Ship".into()), list(statuses))
            .unwrap_err()
    }

    /// Blocking the same todo a second time, after it was retried, asks for
    /// one question to the user. Sending a list where it is still blocked
    /// does not count again.
    #[test]
    fn blocked_twice_requests_user_question() {
        let controller = controller();
        let first = controller
            .write_todos(Some("Ship".into()), vec![blocked("t1", "missing input")])
            .unwrap();
        assert!(!first.should_ask_user);
        let still = controller
            .write_todos(None, vec![blocked("t1", "missing input")])
            .unwrap();
        assert!(!still.should_ask_user);

        controller.write_todos(None, list(&[InProgress])).unwrap();
        let second = controller
            .write_todos(None, vec![blocked("t1", "still missing")])
            .unwrap();
        assert!(second.should_ask_user);
        assert!(second.message.contains("ask the user"));
    }

    #[test]
    fn verify_requires_all_todos_done_for_success() {
        let controller = controller();
        write(&controller, &[Done, Pending]);

        let error = controller
            .verify_completion(true, "not yet".into(), vec!["t1: done".into()], None)
            .unwrap_err();
        assert!(matches!(error, AgentTaskError::TodosNotDone));
    }

    /// Every todo done, verified as achieved.
    fn verify_done(controller: &AgentTaskController) -> Result<AgentTaskResponse, AgentTaskError> {
        controller.write_todos(None, list(&[Done, Done])).unwrap();
        controller.verify_completion(true, "done".into(), vec!["t1: ok".into()], None)
    }

    /// AGE-2179: a coordinator that never delegated is told to, not passed;
    /// twice at most, so a task its team has no part in still ends.
    #[test]
    fn a_coordinator_that_never_delegated_is_nudged_twice_then_passed() {
        let controller = controller().requiring_delegation(true);
        write(&controller, &[Pending, Pending]);

        let error = verify_done(&controller).unwrap_err();
        assert!(matches!(error, AgentTaskError::NotDelegated));
        assert!(error.to_string().contains("invoke_agent"), "{error}");
        assert!(!controller.snapshot().verified);
        assert!(matches!(
            verify_done(&controller).unwrap_err(),
            AgentTaskError::NotDelegated
        ));
        assert!(verify_done(&controller).unwrap().snapshot.verified);
    }

    #[test]
    fn a_coordinator_that_delegated_passes() {
        let controller = controller().requiring_delegation(true);
        write(&controller, &[Pending, Pending]);
        controller.note_delegation();
        assert!(verify_done(&controller).unwrap().snapshot.verified);
    }

    /// Delegation is per user turn; the requirement is per agent.
    #[test]
    fn a_reset_forgets_the_delegation_but_keeps_the_requirement() {
        let controller = controller().requiring_delegation(true);
        controller.note_delegation();
        controller.reset();
        write(&controller, &[Pending, Pending]);
        assert!(matches!(
            verify_done(&controller).unwrap_err(),
            AgentTaskError::NotDelegated
        ));
    }

    /// A coordinator's turn that ends without a delegation is nudged, even
    /// on a small finished plan (which AGE-2188 lets end unverified) or no
    /// plan at all, under the same cap the verification nudge counts on.
    #[test]
    fn a_coordinator_turn_ending_without_a_delegation_is_nudged() {
        let controller = controller().requiring_delegation(true);
        write(&controller, &[Done]);
        let nudge = controller.stream_end_follow_up().expect("a nudge");
        assert!(nudge.contains("invoke_agent"), "{nudge}");
        assert!(is_protocol_follow_up_text(&nudge));
        assert!(matches!(
            controller.verify_completion(true, "done".into(), vec!["t1: ok".into()], None),
            Err(AgentTaskError::NotDelegated)
        ));
        assert!(
            controller.stream_end_follow_up().is_none(),
            "the cap is spent"
        );

        let controller = AgentTaskController::new().requiring_delegation(true);
        assert!(controller.stream_end_follow_up().is_some(), "no plan");
        controller.note_delegation();
        assert!(controller.stream_end_follow_up().is_none(), "delegated");
        assert!(
            AgentTaskController::new().stream_end_follow_up().is_none(),
            "anyone else"
        );
    }

    /// Every other agent verifies as before.
    #[test]
    fn an_agent_not_required_to_delegate_passes_without_delegating() {
        let controller = controller();
        write(&controller, &[Pending, Pending]);
        assert!(verify_done(&controller).unwrap().snapshot.verified);
    }

    #[test]
    fn verify_false_reopens_done_todos() {
        let controller = controller();
        write(&controller, &[Done, Done, Done]);

        let response = controller
            .verify_completion(
                false,
                "test fails".into(),
                vec!["t3: test still red".into()],
                Some("the fix missed a case".into()),
            )
            .unwrap();

        assert!(
            response
                .snapshot
                .todos
                .iter()
                .all(|todo| todo.status == Pending)
        );
        assert!(!response.snapshot.verified);
    }

    /// No plan, no protocol: a turn that only ever called ordinary tools is
    /// never asked to write todos, verify, or anything else (AGE-479).
    #[test]
    fn no_follow_up_without_a_plan() {
        let controller = controller();

        assert!(controller.stream_end_follow_up().is_none());
        assert!(controller.stream_end_follow_up().is_none());
        assert!(!controller.snapshot().verification_skipped);
    }

    #[test]
    fn protocol_follow_up_text_is_recognized() {
        let controller = controller();
        write(&controller, &[Pending, Pending]);
        let prompt = controller.stream_end_follow_up().unwrap();
        assert!(is_protocol_follow_up_text(&prompt));
        assert!(is_protocol_follow_up_text(
            "LOOP DETECTED: You just called `read_file` with the same arguments twice"
        ));
        // The nudge older transcripts hold.
        assert!(is_protocol_follow_up_text(
            "A todo is still in progress. Call update_todo with status=\"done\""
        ));
        assert!(!is_protocol_follow_up_text("Please summarize the report."));
    }

    #[test]
    fn stream_end_requires_verification_after_todos() {
        let controller = controller();
        let response = write(&controller, &[Done, Done, Done]);
        assert!(response.message.contains("verify_completion"));

        let prompt = controller.stream_end_follow_up();

        assert!(prompt.is_some());
        assert!(prompt.unwrap().contains("verify_completion"));
    }

    /// AGE-2188: a one- or two-todo plan that is all done ends the turn
    /// without an evidence round.
    #[test]
    fn a_small_finished_plan_needs_no_verification() {
        let controller = controller();
        let response = write(&controller, &[Done, Done]);
        assert!(!response.message.contains("verify_completion"));

        assert!(controller.stream_end_follow_up().is_none());
        assert!(!controller.snapshot().verification_skipped);
    }

    /// A small plan left unfinished is still steered: an open todo means
    /// the agent stopped early.
    #[test]
    fn a_small_unfinished_plan_is_still_nudged() {
        let controller = controller();
        write(&controller, &[Done, Pending]);

        assert!(controller.stream_end_follow_up().is_some());
    }

    #[test]
    fn in_progress_todo_asks_for_the_list_not_verification() {
        let controller = controller();
        write(&controller, &[InProgress, Pending]);

        let prompt = controller.stream_end_follow_up().unwrap();

        assert!(prompt.contains("write_todos"));
        assert!(!prompt.contains("verify_completion"));
        assert!(!prompt.contains("update_todo"));
    }

    #[test]
    fn stream_end_follow_up_stops_after_the_cap() {
        let controller = controller();
        write(&controller, &[Pending, Pending]);

        for _ in 0..MAX_PROTOCOL_FOLLOW_UPS {
            assert!(controller.stream_end_follow_up().is_some());
        }

        assert!(controller.stream_end_follow_up().is_none());
        assert!(controller.snapshot().verification_skipped);
    }

    #[test]
    fn reset_clears_state_and_allows_a_new_plan() {
        let controller = controller();
        write(&controller, &[Pending, Pending]);
        for _ in 0..MAX_PROTOCOL_FOLLOW_UPS {
            controller.stream_end_follow_up();
        }
        assert!(controller.stream_end_follow_up().is_none());

        controller.reset();

        let snapshot = controller.snapshot();
        assert!(!snapshot.write_todos_called);
        assert!(!snapshot.verification_skipped);
        assert!(snapshot.goal.is_none());
        write(&controller, &[Pending, Pending]);
        assert!(controller.stream_end_follow_up().is_some());
    }

    #[test]
    fn stream_end_allows_verified_completion() {
        let controller = controller();
        write(&controller, &[Done, Done, Done]);
        controller
            .verify_completion(
                true,
                "all done".into(),
                vec!["t1: done".into(), "t2: done".into(), "t3: done".into()],
                None,
            )
            .unwrap();

        assert!(controller.stream_end_follow_up().is_none());
    }

    fn offered() -> AgentTaskController {
        let controller = controller();
        controller.set_offers_plan(true);
        controller
    }

    /// The reminders' tool calls: every result before the nth is quiet.
    fn calls(controller: &AgentTaskController, n: usize) -> Vec<Option<&'static str>> {
        (0..n)
            .map(|_| controller.note_tool_call("read_file"))
            .collect()
    }

    /// AGE-2189: a turn without a plan hears once, at the threshold, that
    /// `write_todos` exists.
    #[test]
    fn a_long_turn_without_a_plan_is_reminded_once() {
        let controller = offered();
        let notes = calls(&controller, REMIND_AFTER_TOOL_CALLS * 3);

        assert_eq!(notes.iter().flatten().count(), 1);
        assert_eq!(notes[REMIND_AFTER_TOOL_CALLS - 1], Some(NO_PLAN_REMINDER));
        assert!(NO_PLAN_REMINDER.contains("write_todos"));
    }

    #[test]
    fn no_reminder_when_write_todos_is_not_offered() {
        let controller = controller();
        assert!(
            calls(&controller, REMIND_AFTER_TOOL_CALLS * 2)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn todo_tools_do_not_count_toward_a_reminder() {
        let controller = offered();
        for _ in 0..REMIND_AFTER_TOOL_CALLS * 2 {
            assert!(controller.note_tool_call("verify_completion").is_none());
        }
    }

    /// A plan with open todos that has not been sent again for the
    /// threshold hears once that it is stale; the next write re-arms it.
    #[test]
    fn a_stale_plan_is_reminded_once_per_stretch() {
        let controller = offered();
        write(&controller, &[InProgress, Pending]);

        let notes = calls(&controller, REMIND_AFTER_TOOL_CALLS * 2);
        assert_eq!(notes.iter().flatten().count(), 1);
        assert_eq!(
            notes[REMIND_AFTER_TOOL_CALLS - 1],
            Some(STALE_PLAN_REMINDER)
        );

        write(&controller, &[Done, InProgress]);
        let notes = calls(&controller, REMIND_AFTER_TOOL_CALLS);
        assert_eq!(notes.last().copied().flatten(), Some(STALE_PLAN_REMINDER));
    }

    #[test]
    fn a_finished_plan_is_not_reminded() {
        let controller = offered();
        write(&controller, &[Done, Done]);
        assert!(
            calls(&controller, REMIND_AFTER_TOOL_CALLS * 2)
                .iter()
                .all(Option::is_none)
        );
    }

    /// A new user turn starts the count again; whether the agent is offered
    /// `write_todos` is the agent's, not the turn's, and survives.
    #[test]
    fn reset_rearms_the_reminder_and_keeps_the_offer() {
        let controller = offered();
        calls(&controller, REMIND_AFTER_TOOL_CALLS);
        controller.reset();
        let notes = calls(&controller, REMIND_AFTER_TOOL_CALLS);
        assert_eq!(notes.last().copied().flatten(), Some(NO_PLAN_REMINDER));
    }

    /// End to end through rig: the hook puts the reminder on the result of
    /// the call that reaches the threshold, and on no other.
    #[tokio::test]
    async fn the_hook_puts_the_reminder_on_the_threshold_result() {
        use futures::StreamExt;
        use rig_agent::agent::AgentBuilder;
        use rig_agent::test_utils::MockAddTool;
        use rig_core::completion::Message;
        use rig_core::message::UserContent;
        use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

        let n = REMIND_AFTER_TOOL_CALLS;
        let mut turns: Vec<Vec<MockStreamEvent>> = (0..n)
            .map(|i| {
                vec![
                    MockStreamEvent::tool_call(
                        format!("tool_call_{i}"),
                        "add",
                        serde_json::json!({"x": 1, "y": 2}),
                    )
                    .with_call_id(format!("call_{i}")),
                    MockStreamEvent::final_response_with_total_tokens(4),
                ]
            })
            .collect();
        turns.push(vec![
            MockStreamEvent::text("3"),
            MockStreamEvent::final_response_with_total_tokens(4),
        ]);
        let model = MockCompletionModel::from_stream_turns(turns);
        let agent = AgentBuilder::new(model.clone()).tool(MockAddTool).build();
        let controller = offered();

        let mut stream = agent
            .prompt("add")
            .max_turns(n + 2)
            .add_hook(TodoReminder(controller))
            .stream();
        while let Some(item) = stream.next().await {
            item.expect("the run ends without an error");
        }

        let result_text = |request: usize| -> String {
            let requests = model.requests();
            let last = requests[request].chat_history.last().cloned().unwrap();
            let Message::User { content } = &*last else {
                panic!("request {request} ends on a tool result")
            };
            content
                .iter()
                .filter_map(|c| match c {
                    UserContent::ToolResult(r) => Some(
                        r.content
                            .iter()
                            .map(|b| match (b.as_text(), b.as_json()) {
                                (Some(text), _) => text.to_owned(),
                                (None, Some(value)) => value.to_string(),
                                (None, None) => String::new(),
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect()
        };
        for request in 1..n {
            assert_eq!(result_text(request), "3", "request {request}");
        }
        assert_eq!(result_text(n), format!("3\n\n{NO_PLAN_REMINDER}"));
    }
}
