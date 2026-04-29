# Codebase Notes

- chatty2 is a Rust + GPUI desktop app, not a DOM/CSS app. The subtle guidance brief's web-only primitives are represented with GPUI-native opacity animations and fixed layout reservations.
- `crates/chatty-gpui/src/feature_flags.rs` contains compile-time/env feature gates. A/B flags use deterministic seed buckets when the environment value is `ab`, `50`, `50%`, or `rollout`.
- `crates/chatty-gpui/src/subtle_guidance.rs` is the single pure-helper home for motion constants, reduced-motion lookup, phase copy, skeleton heuristics, trace-chip formatting, and stream-smoothing pacing.
- `ChatView::append_assistant_text` is the stream-smoothing interception point; reduced motion bypasses buffering and writes provider chunks immediately.
- `GeneralSettingsModel::show_tool_traces_live` defaults to `false` and is the user override that restores live trace rendering when `subtle_guidance_traces` is enabled.
