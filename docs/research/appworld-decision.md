# AppWorld / Stage B environments (AGE-24 pivot)

**Decision (updated 2026-08-26):** Stage B sandboxes — including any future AppWorld
slice — run via **Harbor** in the sibling repo `harbor-chatty`
(`~/Documents/chattyapp/harbor-chatty`), not via in-repo Python bridges in `chatty-eval`.

**M4 Stage B for now:** FiNER (entity exact-match) plus synthetic tool-use that exercises
ACE’s Generator / Reflector / Curator loop. AppWorld remains **cited, not reproduced**,
until FiNER + calibration are green; then add an AppWorld Harbor task adapter (AGE-34),
not a `chatty-core` subprocess bridge.

See Linear AGE-24 (Harbor pivot) and AGE-34.
