# Performance Journal

## 2026-04-28 — Phase 1

**What shipped:** Added `subtle_guidance_v1` flag plumbing and an opt-in P0 acknowledgment path for active conversations. When enabled, sending immediately marks the composer as working, inserts the user message plus an assistant reservation slot, and renders a quiet breathing caret instead of status-chip loading chrome.

**Metrics:** Synthetic DOM harness is not yet available in this GPUI desktop app, so no chart was generated in this phase. Manual acceptance should verify the first assistant slot paint happens on the send event path before async model preparation.

**What surprised me:** Conversation creation already has synchronous UI work, but first-send streaming still waits for the async conversation task before the chat turn can proceed.

**What's next:** Phase 2 should add backend-driven phase whisperer events and the composer hairline, plus a native-compatible synthetic measurement harness.
