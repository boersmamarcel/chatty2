# AGE-425 plan

## Acceptance criteria
- .claude/skills/gpui and .claude/skills/gpui-component are symlinks (git mode 120000) pointing at ../../.agents/skills/<name>.
- `diff -rq .agents/skills/gpui .claude/skills/gpui` and the same for gpui-component print nothing.
- .agents/skills/ is unchanged.
- Only the two .claude/skills/ paths change. The .chatty files are removed later.

## Steps
1. [x] Verify the directories are identical (and not already symlinks); if not, change nothing and report NOT DONE.
2. [x] (commit: this one, see git log) For each name: git rm -r, ln -s ../../.agents/skills/<name>, git add; no leftover untracked files.
3. [x] grep references to skills/gpui (excluding skills dirs); check they resolve; do not edit.
