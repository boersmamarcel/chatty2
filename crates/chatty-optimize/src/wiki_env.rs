//! WikiEnv adapter over chatty-core search/fetch tools (AGE-11).
//!
//! The agent loop is not rewritten here — this is an eval-time environment façade.

use serde::{Deserialize, Serialize};

/// Scripted or live Wikipedia-backed environment for ReAct knowledge-intensive eval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiEnvConfig {
    pub max_searches: usize,
    /// When true, use fixture responses instead of live HTTP (unit tests / CI).
    pub scripted: bool,
}

impl Default for WikiEnvConfig {
    fn default() -> Self {
        Self {
            max_searches: 8,
            scripted: true,
        }
    }
}

/// Observation returned after a search/lookup/finish action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiObservation {
    pub text: String,
    pub done: bool,
}

/// Eval harness handle. Live wiring uses chatty-core `search_tool` / `fetch_tool`.
pub struct WikiEnv {
    pub config: WikiEnvConfig,
    searches: usize,
}

impl WikiEnv {
    pub fn new(config: WikiEnvConfig) -> Self {
        Self {
            config,
            searches: 0,
        }
    }

    pub fn search(&mut self, query: &str) -> WikiObservation {
        self.searches += 1;
        if self.config.scripted {
            WikiObservation {
                text: format!("[scripted search] {query}"),
                done: false,
            }
        } else {
            WikiObservation {
                text: format!("[live search not wired yet] {query}"),
                done: false,
            }
        }
    }

    pub fn finish(&mut self, answer: &str) -> WikiObservation {
        WikiObservation {
            text: answer.to_string(),
            done: true,
        }
    }

    pub fn search_count(&self) -> usize {
        self.searches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_search_and_finish() {
        let mut env = WikiEnv::new(WikiEnvConfig::default());
        let obs = env.search("Paris");
        assert!(!obs.done);
        assert_eq!(env.search_count(), 1);
        let done = env.finish("Paris");
        assert!(done.done);
    }
}
