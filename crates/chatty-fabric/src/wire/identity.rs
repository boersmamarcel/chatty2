//! Whose task it is (ADR-0021 § 2, EN-3a).

use serde::{Deserialize, Serialize};

/// The tenant and user a task runs for, carried to the worker that runs it.
///
/// It replaces the caller's bearer token the `task.run` used to carry
/// (AGE-371): a credential never rides the worker socket. The host edge
/// validates the caller's bearer and stamps this; a hosted worker runs the
/// task as that user, and a hosted root answers an approval only for a
/// request from the same `(tenant, user)`. A local worker has no use for it,
/// and a desktop root sends none.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskIdentity {
    pub tenant: String,
    pub user: String,
}

impl TaskIdentity {
    pub fn new(tenant: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            tenant: tenant.into(),
            user: user.into(),
        }
    }
}
