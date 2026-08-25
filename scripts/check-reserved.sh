#!/usr/bin/env bash
# Fails if a function reserved in RESERVED.md exists in a file with neither
# its `todo!("human: ...")` marker nor a `// HUMAN-WRITTEN: <symbol>` attestation.
#
# That combination is what an agent silently implementing a reserved function looks like.
# Passes trivially while the files do not exist yet.
set -uo pipefail
cd "$(dirname "$0")/.."

fail=0
checked=0

# Table rows look like: | `path` | `symbol` | ISSUE | why |
while IFS= read -r line; do
  path=$(printf '%s' "$line" | sed -n 's/^| *`\([^`]*\)` *| *`\([^`]*\)`.*/\1/p')
  sym=$(printf '%s'  "$line" | sed -n 's/^| *`\([^`]*\)` *| *`\([^`]*\)`.*/\2/p')
  [ -z "$path" ] && continue
  [ -z "$sym" ]  && continue
  case "$path" in */*) ;; *) continue ;; esac   # skip non-path rows
  [ -f "$path" ] || continue                    # not written yet: fine
  checked=$((checked + 1))
  if grep -qF 'todo!("human:' "$path"; then
    continue
  fi
  if grep -qF "HUMAN-WRITTEN: $sym" "$path"; then
    continue
  fi
  echo "RESERVED VIOLATION: $path defines reserved symbol '$sym' with no todo!(\"human:\") marker"
  echo "  and no '// HUMAN-WRITTEN: $sym' attestation."
  echo "  Either this was implemented by an agent (not allowed — see RESERVED.md),"
  echo "  or the human wrote it and forgot the attestation comment."
  fail=1
done < RESERVED.md

if [ "$fail" -eq 0 ]; then
  echo "reserved-symbol check: OK ($checked file(s) present and marked)"
fi
exit "$fail"
