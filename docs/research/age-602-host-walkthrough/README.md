# PL-E7 host walkthrough evidence (AGE-602)

Screenshots and a transcript backing the S4 results table in the "WASM
plugin system: evaluation plan" Linear document. All runs used a wiremock
OpenAI-compatible fake as the provider — no real model or the internet.

- `4.5-4.8-start-modules1-agents0.png` — GPUI welcome screen with a
  hand-installed `echo-agent` showing `modules 1` but `agents 0` (F9: the
  module never reaches `list_agents`).
- `4.8-agent-echo-agent-falls-to-subagent.png` — GPUI after sending
  `/agent echo-agent hi`: routes to a headless sub-agent ("Sub-agent…
  echo-agent hi"), not the module.
- `4.10-settings-extensions-no-modules-page.png` — Settings → Extensions:
  the only settings surface a WASM module could show up on, and it doesn't
  (no dedicated "Modules" settings page exists in the app).
- `tui-4.9-broker-startup.txt` — `chatty-tui --broker` startup screen with
  `echo-agent` in the configured module dir; the runtime status bar reads
  `module local 0`.
