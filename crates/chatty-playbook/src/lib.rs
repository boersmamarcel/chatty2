//! ACE playbook crate (AGE-9 / AGE-17).
//!
//! # Promises (AGE-26 production bar)
//!
//! - Bounded playbook growth (eviction policy — not an unbounded experiment knob).
//! - Concrete error enum; no panics on input-driven paths.
//! - Zero dependency on `chatty-eval`.
//! - `#![forbid(unsafe_code)]`.
//!
//! # Reserved
//!
//! `apply` and `grow_and_refine` are human-reserved ([`RESERVED.md`](../../../RESERVED.md)).

#![forbid(unsafe_code)]

pub mod merge;
pub mod refine;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlaybookError {
    #[error("playbook not yet available: {0}")]
    NotReady(&'static str),
    #[error("invalid delta: {0}")]
    InvalidDelta(String),
}

/// Sectioned playbook (ACE bullet store). Ordering is BTreeMap-stable for cache.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Playbook {
    pub sections: BTreeMap<String, Vec<Bullet>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bullet {
    pub id: String,
    pub helpful: u32,
    pub harmful: u32,
    pub content: String,
}

/// Curator delta ops — merge is deterministic and non-LLM (`apply`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "UPPERCASE")]
pub enum DeltaOp {
    Add {
        section: String,
        content: String,
    },
    Update {
        id: String,
        helpful_delta: i32,
        harmful_delta: i32,
    },
}

pub fn crate_ready() -> Result<(), PlaybookError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles() {
        crate_ready().unwrap();
    }
}
