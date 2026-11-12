//! The tasks a root process's broker ran, for A2A `tasks/get` (RC-4).
//!
//! Each task is kept from the moment its worker starts until
//! [`RECENT_TASKS`] after it ends, in memory only: a task of an earlier
//! root process, or one that ended longer ago, is not found. A task belongs
//! to its root conversation, and only that conversation's root reads it.
//! Its `contextId` is the handle it ran under, if any.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use chatty_fabric::ConversationScope;
use serde_json::{Value, json};

use super::protocol::TaskState;

/// How long a finished task is still answered for.
pub(crate) const RECENT_TASKS: Duration = Duration::from_secs(10 * 60);

/// One task, as `tasks/get` answers it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TaskRecord {
    scope: ConversationScope,
    caller: String,
    agent: String,
    /// The handle the task ran under: its A2A `contextId`.
    context_id: Option<String>,
    state: TaskState,
    ended_at: Option<SystemTime>,
}

impl TaskRecord {
    /// A task `caller` gave `agent` in `scope`, working.
    pub(crate) fn new(
        scope: ConversationScope,
        caller: &str,
        agent: &str,
        context_id: Option<String>,
    ) -> Self {
        Self {
            scope,
            caller: caller.to_string(),
            agent: agent.to_string(),
            context_id,
            state: TaskState::Working,
            ended_at: None,
        }
    }

    /// The task as an A2A `Task` object.
    pub(crate) fn to_a2a(&self, id: &str) -> Value {
        let mut task = json!({
            "kind": "task",
            "id": id,
            "status": { "state": self.state.to_string() },
            "metadata": { "agent": self.agent, "caller": self.caller },
        });
        if let Some(context_id) = &self.context_id {
            task["contextId"] = json!(context_id);
        }
        task
    }
}

/// Live and recently finished tasks, by A2A task id.
#[derive(Debug, Default)]
pub(crate) struct RecentTasks {
    tasks: HashMap<String, TaskRecord>,
}

impl RecentTasks {
    pub(crate) fn start(&mut self, id: &str, record: TaskRecord) {
        self.tasks.insert(id.to_string(), record);
    }

    /// The task `id` ended in `state` at `now`. A task that already ended
    /// keeps its first end.
    pub(crate) fn end(&mut self, id: &str, state: TaskState, now: SystemTime) {
        if let Some(record) = self.tasks.get_mut(id)
            && record.ended_at.is_none()
        {
            record.state = state;
            record.ended_at = Some(now);
        }
    }

    /// The task `id` of `scope`, if it is live or ended within
    /// [`RECENT_TASKS`] of `now`. Older tasks are forgotten here.
    pub(crate) fn get(
        &mut self,
        id: &str,
        scope: &ConversationScope,
        now: SystemTime,
    ) -> Option<TaskRecord> {
        self.tasks.retain(|_, record| match record.ended_at {
            Some(ended) => now
                .duration_since(ended)
                .map_or(true, |since| since < RECENT_TASKS),
            None => true,
        });
        self.tasks
            .get(id)
            .filter(|record| record.scope == *scope)
            .cloned()
    }
}
