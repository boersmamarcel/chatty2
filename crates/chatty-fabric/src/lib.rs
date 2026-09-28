//! The broker's state, with no transport in it (ADR-0020, fabric spec §3.1).
//!
//! One broker per root process owns a [`Directory`] of the nodes it admitted,
//! a [`TaskTable`] of the runs between them, a [`PendingList`] of messages
//! per recipient and an [`EdgeLog`] of every task, message and refusal.
//! Callers reach other agents through a [`Transport`], whether that is a
//! direct handle in the root process or a worker's broker-made connection.
//!
//! This crate is pure: both `chatty-protocol-gateway` and `chatty-core`
//! depend on it, so it depends on neither, and never on axum, wasmtime,
//! hive-client, gpui or reqwest (`tests/no_heavy_deps.rs`). Payloads that are
//! chatty-core types (`InvokeAgentProgress`, conversations, usage) travel as
//! [`serde_json::Value`]; chatty-core converts them at its edge.

mod directory;
mod edge_log;
mod origin;
mod pending;
mod task_table;
mod transport;

pub use directory::{
    ConversationScope, Directory, DirectoryError, Node, NodeId, NodeName, NodeState, ROOT_NAME,
};
pub use edge_log::{EdgeKind, EdgeLog, EdgeRow, MAX_EDGE_LOG_BYTES};
pub use origin::AgentOrigin;
pub use pending::{Message, PENDING_LIST_BYTES, PendingList, SENDER_ALLOWANCE_BYTES, wrap_message};
pub use task_table::{RunId, TaskEntry, TaskTable, TaskTableError};
pub use transport::{
    CallError, CallEvent, CallRequest, CallStream, InvokeAgentOutcome, InvokeAgentParams,
    MessageStatus, RefusalReason, SendMessageParams, SpawnContext, Transport,
};
