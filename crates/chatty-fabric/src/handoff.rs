//! A role's handoff contract as it crosses the fabric (TD-2, AGE-693).
//!
//! A team file can name a JSON Schema per role (`team.json` `handoffs`).
//! The broker hands the worker it spawns for that role the schema on its
//! task, and the worker's final answer has to carry one fenced `json` block
//! matching it. Validation lives in `chatty-core`; this is only the shape
//! both ends of the socket agree on.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::wire::Opaque;

/// The role a worker answers as and the JSON Schema its handoff must match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HandoffContract {
    /// The role's agent spec name, as the team file lists it.
    pub role: String,
    /// The schema, already read from the team directory and checked to
    /// compile when the team loaded. Opaque to the broker: only the worker's
    /// validator reads it.
    pub schema: Opaque,
}
