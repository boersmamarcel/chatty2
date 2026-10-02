//! The broker's state, with no transport in it (ADR-0020, fabric spec §3.1).
//!
//! One broker per root process owns a [`Directory`] of the nodes it admitted,
//! a [`TaskTable`] of the runs between them, a [`PendingList`] of messages
//! per recipient and an [`EdgeLog`] of every task, message and refusal.
//! Callers reach other agents through a [`Transport`], whether that is a
//! direct handle in the root process or a worker's broker-made connection.
//! An [`EndpointBudget`] meters how many runs talk to one model endpoint at
//! once, and a [`RunPermit`] holds a run's slot only while the run is not
//! waiting on its own calls (§3.5).
//!
//! This crate is pure: both `chatty-protocol-gateway` and `chatty-core`
//! depend on it, so it depends on neither, and never on axum, wasmtime,
//! hive-client, gpui or reqwest (`tests/no_heavy_deps.rs`). Payloads that are
//! chatty-core types (`InvokeAgentProgress`, conversations, usage) travel as
//! [`serde_json::Value`]; chatty-core converts them at its edge.

mod approval;
mod delegation;
mod directory;
mod edge_log;
mod handoff;
mod origin;
mod pending;
mod permit;
mod question;
mod swarm;
mod task_table;
mod transport;

pub use approval::{ApprovalKind, ApprovalRequest, ApprovalVerdict, Asker};
pub use delegation::{CallChain, CallPolicy, MAX_DEPTH, Refusal, Remaining, deadline_grace};
pub use directory::{
    ConversationScope, Directory, DirectoryError, Node, NodeId, NodeName, NodeState, ROOT_NAME,
};
pub use edge_log::{EdgeKind, EdgeLog, EdgeRow, MAX_EDGE_LOG_BYTES, UsagePricer};
pub use handoff::HandoffContract;
pub use origin::AgentOrigin;
pub use pending::{Message, PENDING_LIST_BYTES, PendingList, SENDER_ALLOWANCE_BYTES, wrap_message};
pub use permit::{
    ChildCall, DEFAULT_ENDPOINT_LIMIT, EndpointBudget, EndpointPermit, RunPermit, RunPermitState,
    WeakRunPermit,
};
pub use question::{Answer, AskReply, AskRequest, Question, QuestionOrigin};
pub use swarm::{FORWARD_INTERVAL, SwarmBatcher, SwarmEvent, SwarmItem};
pub use task_table::{RunId, TaskEntry, TaskTable, TaskTableError};
pub use transport::{
    CANCELLED_BY_USER, CallError, CallEvent, CallRequest, CallStream, InvokeAgentOutcome,
    InvokeAgentParams, MessageStatus, RefusalReason, SendMessageParams, SpawnContext, Transport,
};
