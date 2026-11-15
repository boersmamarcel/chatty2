# AGE-1436 plan

Acceptance criteria:
- Workspace Cargo.toml has no rig-mcp or rig-tap entry and no "Trace capture for AGE-5" comment.
- scripts/check-rig-pins.sh checks rig-core and rig-agent only, prints "rig-pin check: OK".
- chatty-core (four lines) and chatty-gpui (two lines) use flate2.workspace = true / tar.workspace = true.
- cargo metadata --locked succeeds and Cargo.lock is unchanged.
- cost.rs line 90 and the pilot-pending assertion are untouched.
- Only the five listed files change (plus .chatty state files, removed before the PR); CI green.

Steps:
1. [x] Make all manifest/script/comment edits in one commit. (commit: this one)
