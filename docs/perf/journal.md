# Performance Journal

## 2026-04-28 — Phase 1

**What shipped:** Added `subtle_guidance_v1` flag plumbing and an opt-in P0 acknowledgment path for active conversations. When enabled, sending immediately marks the composer as working, inserts the user message plus an assistant reservation slot, and renders a quiet breathing caret instead of status-chip loading chrome.

**Metrics:** Synthetic DOM harness is not yet available in this GPUI desktop app, so no chart was generated in this phase. Manual acceptance should verify the first assistant slot paint happens on the send event path before async model preparation.

**What surprised me:** Conversation creation already has synchronous UI work, but first-send streaming still waits for the async conversation task before the chat turn can proceed.

**What's next:** Phase 2 should add backend-driven phase whisperer events and the composer hairline, plus a native-compatible synthetic measurement harness.

## 2026-04-29 — Phases 2–6 escalation (no code shipped)

**Status:** `escalation-needed` — session halted before any phase-2 work.

**Why:** Two stop conditions from the session brief are met simultaneously.

1. **The authoritative spec is missing from the repo.** The session brief names `docs/perf/chatty2-subtle-visual-guidance-brief.md` as the source of truth and references §4.2, §4.3, §4.4, §4.5, §4.6, §4.7, and §7 by section number for non-negotiable copy strings ("Recalling context", "Choosing tools", "Thinking"), tunable ranges (10–25% opacity, 1.6 s period, ≥30 char/s, ≤90 char/s, ~80-char buffer, ≤200 ms total drift, the long-answer heuristic, the chip text format), and an anti-pattern checklist that every phase must clear. None of those sections can be read — the file is not in `docs/perf/` and is not in git history. `docs/perf/codebase-notes.md` is also absent, so the prep step "read codebase-notes" cannot be performed. Synthesizing those ranges and copy strings would be substituting confidence for evidence, which the brief explicitly forbids.

2. **The brief's primitives don't exist in this codebase.** chatty2 is a Rust + GPUI desktop app (see `crates/chatty-gpui`), and phase 1 confirmed in this same journal that "Synthetic DOM harness is not yet available in this GPUI desktop app." The phase 2–6 brief is written against web primitives: `transform: translateX` on a gradient child as the only allowed strip animation, `requestAnimationFrame` for stream smoothing, `prefers-reduced-motion`, `role="status"` + `aria-live="polite"` + `aria-hidden` + `aria-busy`, WCAG AA contrast checks on themed CSS, CLS = 0 on stream end, and an A/B 50/50 client flag rollout. GPUI has none of these: no DOM, no CSS cascade, no rAF, no ARIA tree, no CLS, and no in-app A/B framework. Phase 1's `feature_flags.rs` is a static-default module, not a rollout system. Implementing phases 2–6 would require either (a) inventing GPUI equivalents for every primitive (which is "diverging from the brief's spec rather than its tunables"), or (b) building the missing harness, A/B layer, accessibility tree, and motion-preference plumbing first — each of which is itself a shared-module / new-flag / new-dependency change the brief doesn't authorize.

**Stop conditions triggered (from the session brief's "When to stop" list):**

- #6 — "The brief contradicts itself or the codebase, or proceeding would require diverging from the brief's spec rather than its tunables." (Both halves apply: the spec is unreadable, and where its intent is inferable it contradicts the platform.)
- #2 — "You'd need to … add a flag the brief doesn't mention, or break previously-shipped phase code." (Bridging web→desktop would require new flags and new platform primitives not in the brief.)

**Proposed resolution (for the human):**

Pick one of:

- **A.** Commit `docs/perf/chatty2-subtle-visual-guidance-brief.md` and `docs/perf/codebase-notes.md` to the repo, then restart the session. This is the cleanest path and matches how the brief describes itself.
- **B.** Re-scope phases 2–6 explicitly for GPUI: replace web primitives with GPUI equivalents (animation = GPUI transitions, reduced-motion = OS setting via `gpui::App`, a11y = platform accessibility APIs, A/B = local 50/50 on a stable hash of conversation id, metrics harness = a new `chatty-perf` test bin), and re-issue the brief with those substitutions baked in plus concrete tunables. The new brief should also drop the CLS/TTFT acceptance bars or replace them with frame-time / first-paint-from-send equivalents that the desktop harness can actually measure.
- **C.** Keep the brief web-centric and split chatty2's chat surface into a web target first; defer phases 2–6 until that exists. (Largest scope, mentioned only for completeness.)

**What I did not do, intentionally:** No source files were modified. No new branch was created (I am on `copilot/implement-phases-2-to-6` as provided). No feature flags added. No CHANGELOG entry written. The phase 1 `subtle_guidance_v1` flag and caret slot remain the only shipped subtle-guidance code; nothing in this escalation regresses it.

**Risk if overridden and we proceed anyway:** every "non-negotiable" item in the brief (copy strings, tunable ranges, anti-pattern list, acceptance gates) becomes a guess. The phase-end journal entries would have nothing to reconcile against, and the final PR would be unreviewable in the way the session brief explicitly warns about ("failures compound and become unreviewable").

## 2026-04-29 — Phases 2–6 session plan

**Phase 2 plan:** I expect to touch `crates/chatty-gpui/src/subtle_guidance.rs`, `feature_flags.rs`, `message_component.rs`, `chat_input.rs`, `message_ops.rs`, `CHANGELOG.md`, and the perf docs. The feature flag is additive to `subtle_guidance_v1`. Tests will encode the 250 ms phase anti-flicker threshold, the 600 ms pre-stream anti-noise threshold, non-negotiable copy (`Recalling context`, `Choosing tools`, `Thinking`), and reduced-motion helper behavior. Acceptance criteria to verify: no "Loading" copy, GPUI-native composer hairline, phase text beside the caret, reduced-motion static fallback, and no regression to phase-1 immediate reservation.

**Phase 3 plan:** I expect to touch `subtle_guidance.rs`, `message_component.rs`, perf docs, and changelog. The flag is `subtle_guidance_skeleton` with deterministic 50/50 A/B bucketing. Tests will cover likely-long heuristics and deterministic 100/80/55% width bands. Acceptance criteria to verify: skeleton renders only for likely-long empty assistant slots, reduced-motion disables shimmer, and first content replaces skeleton without a separate pop/crossfade path.

**Phase 4 plan:** I expect to touch `subtle_guidance.rs`, `message_component.rs`, `trace_components.rs` if needed, `GeneralSettingsModel`, settings controller/view files, tests, perf docs, and changelog. The flag is `subtle_guidance_traces`, with `Show tool traces live` defaulting off. Tests will cover chip formatting from the existing `SystemTrace`. Acceptance criteria to verify: live trace list is not the default waiting visual, dot + whisperer indicate active trace state, finalized trace chip derives from the same trace data, and the setting restores existing live behavior.

**Phase 5 plan:** I expect to touch `subtle_guidance.rs`, `chat_view.rs`, tests, perf docs, and changelog. The flag is `stream_smoothing_v1` with deterministic 50/50 A/B bucketing. Tests will cover the `clamp(round(buffer/4), 1, 3)` equivalent and baseline/accelerated character-rate constants. Acceptance criteria to verify: actual first-token state is unchanged, buffering only happens after first token, no spinner appears when the buffer drains, finalization flushes the buffer, and reduced motion bypasses smoothing.

**Phase 6 plan:** I expect to audit every phase-2–5 motion site and keep all reduced-motion decisions routed through `subtle_guidance::use_reduced_motion()`. Tests will cover static fallback helpers where possible. Acceptance criteria to verify: every motion-using component has an inline fallback comment, focus remains on the composer, theme-derived colors are used for contrast, and DOM-only ARIA/CLS requirements are documented as GPUI platform divergences rather than silently claimed.

## 2026-04-29 — Phase 2 plan

1. Add GPUI subtle guidance helper constants and phase copy.
2. Add feature-flag helpers for later phases while preserving phase-1 `subtle_guidance_v1`.
3. Render whisperer copy inside the existing assistant reservation slot.
4. Render a GPUI-native ambient strip on the composer bottom edge.
5. Route caret/dot/strip motion through a single reduced-motion helper.
6. Add unit tests for anti-flicker, anti-noise, and copy.
7. Capture a desktop play-by-play artifact because the DOM harness is absent.
8. Update changelog and journal.
9. Run quality gates and fix failures before phase 3.

**What shipped:** Phase 2 shipped GPUI-native phase whisperer copy and composer ambient strip behind `subtle_guidance_v1`, with reduced-motion static fallbacks and pure tests for anti-flicker/anti-noise/copy constants.

**Metrics delta:** Synthetic browser chart unavailable in this GPUI desktop repo. Encoded values: phase anti-flicker = 250 ms, pre-stream anti-noise = 600 ms, breathe period = 1.6 s, opacity range = 10–25%. Play-by-play: `docs/perf/2026-04-29-phase-2.play-by-play.md`.

**What surprised me:** The authoritative brief and codebase notes were missing from the repo, so the user-provided problem statement became the only available phase-2–6 spec.

**Risks I'm carrying into the next phase:** GPUI does not expose CSS transform or ARIA primitives; I am implementing native equivalents and documenting divergences instead of claiming browser behavior.

## 2026-04-29 — Phase 3 plan

1. Add deterministic A/B bucketing for `subtle_guidance_skeleton`.
2. Add likely-long answer heuristic helpers.
3. Add deterministic skeleton width jitter.
4. Render skeletons only in empty assistant slots.
5. Keep skeletons in one overflow-hidden container.
6. Add reduced-motion static skeleton fallback.
7. Add pure tests for heuristic and width ranges.
8. Capture play-by-play and update changelog/journal.
9. Re-run quality gates and prior-phase tests.

**What shipped:** Phase 3 shipped deterministic long-answer skeleton reservations behind `subtle_guidance_skeleton`, using GPUI ghost lines with width jitter and reduced-motion static fallback.

**Metrics delta:** DOM CLS chart unavailable. Intended stream-end CLS equivalent is 0 because the skeleton only renders while assistant content is empty and the same slot is reused. Play-by-play: `docs/perf/2026-04-29-phase-3.play-by-play.md`.

**What surprised me:** Existing `gpui_component::skeleton::Skeleton` was generic loading chrome, so the phase needed custom low-contrast lines to respect the brief's subtle-copy constraints.

**Risks I'm carrying into the next phase:** The long-answer heuristic is conservative and local because the missing brief's exact heuristic could not be read.

## 2026-04-29 — Phase 4 plan

1. Add `subtle_guidance_traces` flag helper.
2. Add `show_tool_traces_live` persisted setting defaulting off.
3. Add settings UI switch and persistence controller.
4. Suppress live trace interleaving during wait when subtle trace mode is active.
5. Use existing live trace data to choose whisperer phase and dot presence.
6. Format finalized trace chips from `SystemTrace`.
7. Keep existing trace view as expandable detail below the chip.
8. Add chip-format tests.
9. Capture play-by-play and update changelog/journal.
10. Run quality gates and prior scenarios.

**What shipped:** Phase 4 shipped subtle trace mode behind `subtle_guidance_traces`, a persisted `Show tool traces live` setting defaulting off, active trace dot/phase behavior, and trace chips derived from the existing `SystemTraceView` data.

**Metrics delta:** No separate trace source was added; chip generation is O(trace items). Play-by-play: `docs/perf/2026-04-29-phase-4.play-by-play.md`.

**What surprised me:** The current finalized trace path is interleaved into message rendering; preserving an expandable detail path without new state is simplest by rendering the existing trace view under the chip.

**Risks I'm carrying into the next phase:** The chip expansion affordance is GPUI-native rather than a bespoke web disclosure control.

## 2026-04-29 — Phase 5 plan

1. Add `stream_smoothing_v1` deterministic A/B flag helper.
2. Add stream-smoothing constants and tests.
3. Intercept assistant text appends after first-token state has flipped.
4. Buffer provider chunks per active `ChatView`.
5. Drain buffer on a 16 ms GPUI/Tokio frame loop using `clamp(buffer/4, 1, 3)`.
6. Avoid any spinner when the buffer empties.
7. Flush pending buffer on stream finalization.
8. Bypass smoothing under reduced motion.
9. Capture play-by-play and update journal/changelog.
10. Run quality gates and prior scenarios.

**What shipped:** Phase 5 shipped a guarded stream-smoothing buffer in `ChatView`, with first-token state unchanged, 16 ms frame draining, finalization flush, no spinner, and reduced-motion bypass.

**Metrics delta:** Browser rAF/TTFT chart unavailable. Encoded constants: 30 cps baseline, 90 cps accelerated after >80 buffered chars, 1–3 chars per frame. Play-by-play: `docs/perf/2026-04-29-phase-5.play-by-play.md`.

**What surprised me:** GPUI has no browser `requestAnimationFrame`; the closest native implementation is a Tokio 16 ms loop updating the entity on the UI context.

**Risks I'm carrying into the next phase:** Total stream extension depends on provider chunk burstiness; finalization flush bounds correctness but not visual smoothness in every case.

## 2026-04-29 — Phase 6 plan

1. Audit every new animation call.
2. Ensure all motion decisions use `subtle_guidance::use_reduced_motion()`.
3. Add inline fallback comment blocks at each motion component.
4. Verify stream smoothing bypasses reduced motion.
5. Verify composer focus is not moved during waiting.
6. Use theme-derived muted/accent colors for whisperer and skeleton.
7. Document GPUI ARIA/CLS divergences.
8. Capture play-by-play and update journal/changelog.
9. Run full quality gates and validation.

**What shipped:** Phase 6 shipped the reduced-motion audit: caret, dot, strip, skeleton, and stream smoothing all route through one helper and carry inline fallback comments.

**Metrics delta:** Browser a11y/CLS chart unavailable. Centralized reduced-motion paths are covered by pure tests and code audit. Play-by-play: `docs/perf/2026-04-29-phase-6.play-by-play.md`.

**What surprised me:** The phase's ARIA requirements are DOM-specific; GPUI does not expose role/status/aria-live/aria-busy attributes in this codebase.

**Risks I'm carrying into the next phase:** The final PR needs reviewer attention on platform divergences from DOM-specific parts of the brief.
