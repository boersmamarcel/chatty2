# Prepare a workspace that looks like a checked-out feature branch of a GitHub
# repository, and stub the `gh` CLI the PR status bar shells out to.
#
# Nothing here reaches the network: the bar's real code path runs (read the
# origin remote, read the branch, ask `gh` for the branch's PR), only the
# answer is scripted — the same trick mock_ollama.py plays on the model.
WS="$RUN_DIR/workspace"

git -C "$WS" init -q -b claude/pr-status-topbar-github-link
git -C "$WS" config user.email "demo@example.com"
git -C "$WS" config user.name "Chatty Demo"
git -C "$WS" remote add origin https://github.com/boersmamarcel/chatty2.git
git -C "$WS" add -A
git -C "$WS" -c commit.gpgsign=false commit -qm "Add the PR status bar"

# `gh pr view --json …`, answered from a script instead of from GitHub.
cat > "$RUN_DIR/bin/gh" <<'STUB'
#!/usr/bin/env bash
[[ "$1" == "pr" && "$2" == "view" ]] || exit 1
cat <<'JSON'
{
  "number": 591,
  "title": "PR status topbar: link the chat to the workspace's GitHub pull request",
  "state": "OPEN",
  "isDraft": false,
  "url": "https://github.com/boersmamarcel/chatty2/pull/591",
  "additions": 964,
  "deletions": 12,
  "statusCheckRollup": [
    {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS",
     "detailsUrl": "https://github.com/boersmamarcel/chatty2/actions/runs/1"},
    {"__typename": "CheckRun", "name": "fmt", "status": "COMPLETED", "conclusion": "SUCCESS",
     "detailsUrl": "https://github.com/boersmamarcel/chatty2/actions/runs/2"},
    {"__typename": "CheckRun", "name": "clippy", "status": "COMPLETED", "conclusion": "SUCCESS",
     "detailsUrl": "https://github.com/boersmamarcel/chatty2/actions/runs/3"}
  ]
}
JSON
STUB
chmod +x "$RUN_DIR/bin/gh"
