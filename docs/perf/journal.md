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

