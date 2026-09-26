//! GEPA reflection meta-prompt (AGE-15).
//!
//! `REFLECTION_META_PROMPT` is **reserved**. Do not quote Appendix B here.
//!
//! # Reference
//!
//! Agrawal et al., *GEPA: Reflective Prompt Evolution Can Outperform Reinforcement
//! Learning*, ICLR 2026 Oral, arXiv:2507.19457. The wording lives in **Appendix B**;
//! Observations 1–2 discuss why that text yields declarative instructions rather than
//! MIPROv2-style quasi-exemplars. Read the appendix; do not paste it into this file.
//! <https://arxiv.org/abs/2507.19457>

/// Reflection meta-prompt text — human writes from paper Appendix B reasoning.
pub fn reflection_meta_prompt() -> &'static str {
    todo!("human: REFLECTION_META_PROMPT — Appendix B wording (AGE-15); do not copy blindly")
}

/// Stable name matching RESERVED.md symbol for CI attestation later.
#[allow(non_upper_case_globals)]
pub static REFLECTION_META_PROMPT: &str = "";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: REFLECTION_META_PROMPT")]
    fn reflection_meta_prompt_is_reserved() {
        let _ = reflection_meta_prompt();
    }
}
