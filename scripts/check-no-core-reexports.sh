#!/usr/bin/env bash
# Fails if chatty-gpui or chatty-tui re-export chatty_core's UI-agnostic
# modules (auth, exporters, factories, repositories, tools).
#
# Those wildcard re-exports were removed from crates/chatty-gpui/src/chatty/mod.rs
# because they hid which crate a definition lived in (see docs/refactor-followups.md
# §2d). Call sites must import from `chatty_core::…` directly instead.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
modules='auth|exporters|factories|repositories|tools'

for crate in chatty-gpui chatty-tui; do
  matches=$(grep -rnE "pub use chatty_core::(\{[^}]*\b($modules)\b|($modules)\b)" \
    "crates/$crate/src" 2>/dev/null || true)
  if [ -n "$matches" ]; then
    echo "RE-EXPORT VIOLATION in $crate:"
    echo "$matches"
    echo "  Re-exporting chatty_core's UI-agnostic modules hides which crate a"
    echo "  definition lives in. Import from chatty_core::… at the call site instead."
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  echo "no-core-reexports check: OK"
fi
exit "$fail"
