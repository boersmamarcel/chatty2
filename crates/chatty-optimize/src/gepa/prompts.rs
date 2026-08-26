//! GEPA reflection meta-prompt (AGE-15).
//!
//! `REFLECTION_META_PROMPT` is **reserved**. Do not quote Appendix B here.

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
