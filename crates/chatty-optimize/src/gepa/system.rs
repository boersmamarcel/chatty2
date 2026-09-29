//! `CompoundSystem` seam (AGE-15): GEPA is a layer over any multi-module prompt system.
//!
//! Two toy impls exist so the same `evolve` binary can run against more than one
//! system without an LLM. `µ_f` / [`FeedbackFn`](chatty_trace) is still reserved (AGE-5);
//! `evaluate` is scalar `µ` only.
//!
//! # Reference
//!
//! Agrawal et al., *GEPA: Reflective Prompt Evolution Can Outperform Reinforcement
//! Learning*, ICLR 2026 Oral, arXiv:2507.19457. §3: GEPA optimizes a *compound* system
//! by rewriting one module at a time (`SELECTMODULE`). `µ` vs `µ_f` is the same section.
//! <https://arxiv.org/abs/2507.19457>

/// A compound AI system whose textual modules GEPA can rewrite.
pub trait CompoundSystem: Clone {
    fn n_modules(&self) -> usize;
    fn prompt(&self, module: usize) -> &str;
    fn set_prompt(&mut self, module: usize, prompt: String);
    /// Scalar eval metric `µ` on one instance. Not `µ_f`.
    fn evaluate(&self, instance: &str) -> f64;
}

/// Single-module system (chatty2 `ModelConfig.preamble` shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordSystem {
    pub preamble: String,
}

impl KeywordSystem {
    pub fn new(preamble: impl Into<String>) -> Self {
        Self {
            preamble: preamble.into(),
        }
    }
}

impl CompoundSystem for KeywordSystem {
    fn n_modules(&self) -> usize {
        1
    }

    fn prompt(&self, module: usize) -> &str {
        assert_eq!(module, 0, "KeywordSystem has one module");
        &self.preamble
    }

    fn set_prompt(&mut self, module: usize, prompt: String) {
        assert_eq!(module, 0, "KeywordSystem has one module");
        self.preamble = prompt;
    }

    fn evaluate(&self, instance: &str) -> f64 {
        if self.preamble.contains(instance) {
            1.0
        } else {
            0.0
        }
    }
}

/// Two-module system (placeholder for a multi-hop / `IrRepr` later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DualKeywordSystem {
    pub modules: [String; 2],
}

impl DualKeywordSystem {
    pub fn new(m0: impl Into<String>, m1: impl Into<String>) -> Self {
        Self {
            modules: [m0.into(), m1.into()],
        }
    }
}

impl CompoundSystem for DualKeywordSystem {
    fn n_modules(&self) -> usize {
        2
    }

    fn prompt(&self, module: usize) -> &str {
        &self.modules[module]
    }

    fn set_prompt(&mut self, module: usize, prompt: String) {
        self.modules[module] = prompt;
    }

    fn evaluate(&self, instance: &str) -> f64 {
        let joined = format!("{} {}", self.modules[0], self.modules[1]);
        if joined.contains(instance) { 1.0 } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_system_is_single_module() {
        let mut s = KeywordSystem::new("hello world");
        assert_eq!(s.n_modules(), 1);
        assert_eq!(s.evaluate("hello"), 1.0);
        assert_eq!(s.evaluate("xyz"), 0.0);
        s.set_prompt(0, "xyz".into());
        assert_eq!(s.evaluate("xyz"), 1.0);
    }

    #[test]
    fn dual_system_is_two_modules() {
        let mut s = DualKeywordSystem::new("hop-a", "hop-b");
        assert_eq!(s.n_modules(), 2);
        assert_eq!(s.evaluate("hop-a"), 1.0);
        s.set_prompt(1, "secret".into());
        assert_eq!(s.evaluate("secret"), 1.0);
        assert_eq!(s.prompt(0), "hop-a");
    }
}
