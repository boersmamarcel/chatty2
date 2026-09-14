#!/usr/bin/env bash
# Minimal team smoke test (ADR-0011 C14, AGE-408): run the `coder-reviewer`
# team headless in a plain container against a local Ollama and report the
# verifier's reward. One command, no arguments; see docs/team-smoke-test.md.
#
#   CHATTY_TUI=<path>   use this binary instead of building target/release/chatty-tui
#   OLLAMA_URL=<url>    Ollama as seen from inside the container (default http://172.17.0.1:11434)
#   LEADER_MODEL=<tag>  leader + reviewer model (default qwen3:14b, think off on the leader)
#   CODER_MODEL=<tag>   coder model (default qwen3:4b)
#   TEAM_SMOKE_TIMEOUT  leader wall-clock limit in seconds (default 900)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SMOKE="$ROOT/scripts/team-smoke"
OLLAMA_URL="${OLLAMA_URL:-http://172.17.0.1:11434}"
LEADER_MODEL="${LEADER_MODEL:-qwen3:14b}"
CODER_MODEL="${CODER_MODEL:-qwen3:4b}"
TIMEOUT="${TEAM_SMOKE_TIMEOUT:-900}"
IMAGE=chatty-team-smoke
TASK='Fix Account.withdraw in bank.py. Acceptance criteria: 1. withdraw raises ValueError when the amount is not positive. 2. withdraw raises ValueError when the amount exceeds the balance. 3. Otherwise withdraw subtracts the amount and returns the new balance. 4. tests/test_bank.py gains a test for each of criteria 1-3, so it defines at least four tests. 5. test_deposit keeps passing and deposit does not change. Never edit files yourself.'

# 1. The binary: CHATTY_TUI, else target/release/chatty-tui, rebuilt when
#    missing or older than any source or manifest in the workspace.
if [ -n "${CHATTY_TUI:-}" ]; then
  BIN="$(cd "$(dirname "$CHATTY_TUI")" && pwd)/$(basename "$CHATTY_TUI")"
  [ -x "$BIN" ] || { echo "CHATTY_TUI=$CHATTY_TUI is not an executable" >&2; exit 2; }
else
  BIN="$ROOT/target/release/chatty-tui"
  if [ ! -x "$BIN" ] || [ -n "$(find "$ROOT/crates" "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" \
        \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name '*.json' -o -name '*.md' \) \
        -newer "$BIN" -print -quit)" ]; then
    echo "building target/release/chatty-tui (this takes a while)"
    (cd "$ROOT" && cargo build --release -p chatty-tui)
  fi
fi

# 2. The image: ubuntu:24.04 + python3 + git + ca-certificates.
docker build -q -t "$IMAGE" "$SMOKE" >/dev/null

# 3. A throwaway HOME: Ollama provider, the two models, and the
#    coder-reviewer preset copied into the data dir with the models and the
#    verification command filled in (the compiled-in preset carries neither).
RUN_DIR="$ROOT/target/team-smoke/run-$(date +%Y%m%d-%H%M%S)"
HOME_DIR="$RUN_DIR/home"
WORK_DIR="$RUN_DIR/work"
CFG="$HOME_DIR/.config/chatty"
TEAM_DIR="$HOME_DIR/.local/share/chatty/teams/coder-reviewer"
mkdir -p "$CFG" "$TEAM_DIR" "$WORK_DIR"
cat > "$CFG/providers.json" <<EOF
[{"provider_type": "ollama", "base_url": "$OLLAMA_URL", "name": "Ollama (docker bridge)"}]
EOF
cat > "$CFG/models.json" <<EOF
[
  {"id": "leader", "name": "$LEADER_MODEL", "provider_type": "ollama", "model_identifier": "$LEADER_MODEL",
   "temperature": 0.3, "extra_params": {"think": "false"}},
  {"id": "coder", "name": "$CODER_MODEL", "provider_type": "ollama", "model_identifier": "$CODER_MODEL",
   "temperature": 0.3, "extra_params": {}}
]
EOF
cat > "$CFG/execution_settings.json" <<'EOF'
{
  "enabled": true,
  "approval_mode": "AutoApproveAll",
  "workspace_dir": "/work",
  "filesystem_read_enabled": true,
  "filesystem_write_enabled": true,
  "fetch_enabled": false,
  "git_enabled": true,
  "browser_enabled": false,
  "execute_code_enabled": false,
  "docker_code_execution_enabled": false,
  "docker_host": null,
  "timeout_seconds": 60,
  "max_output_bytes": 1048576,
  "network_isolation": false,
  "max_agent_turns": 50,
  "memory_enabled": false,
  "embedding_enabled": false,
  "hosted_conversations_enabled": false
}
EOF
printf '[user]\n\temail = team-smoke@chatty.invalid\n\tname = team-smoke\n' > "$HOME_DIR/.gitconfig"
cp "$ROOT/crates/chatty-core/teams/coder-reviewer/SKILL.md" "$TEAM_DIR/SKILL.md"
LEADER_MODEL="$LEADER_MODEL" CODER_MODEL="$CODER_MODEL" python3 - \
  "$ROOT/crates/chatty-core/teams/coder-reviewer/team.json" "$TEAM_DIR/team.json" <<'PY'
import json, os, sys
team = json.load(open(sys.argv[1]))
team["leader"]["model"] = os.environ["LEADER_MODEL"]
for agent in team["agents"]:
    agent["model"] = os.environ["CODER_MODEL"] if agent["name"] == "local-coder" else os.environ["LEADER_MODEL"]
team["verification"] = "python3 -m unittest discover -s tests -t . -v"
json.dump(team, open(sys.argv[2], "w"), indent=2)
PY

# 4. The fixture repo: the bank-account module with the overdraft bug.
cp -r "$SMOKE/fixture/." "$WORK_DIR/"
git -C "$WORK_DIR" -c init.defaultBranch=main init -q
git -C "$WORK_DIR" -c user.name=team-smoke -c user.email=team-smoke@chatty.invalid add -A
git -C "$WORK_DIR" -c user.name=team-smoke -c user.email=team-smoke@chatty.invalid commit -q -m init
BASE_COMMIT="$(git -C "$WORK_DIR" rev-parse --short HEAD)"

# 5. The leader, as this user, under a timeout. A process poll every 3 s
#    records each worker process (`--participant-name <role>-<n>`) so the
#    summary can count what was spawned per role.
NAME="chatty-team-smoke-$$"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
echo "run dir: $RUN_DIR"
echo "leader: $LEADER_MODEL  coder: $CODER_MODEL  ollama: $OLLAMA_URL  timeout: ${TIMEOUT}s"
: > "$RUN_DIR/ps.log"
start=$(date +%s)
set +e
# shellcheck disable=SC2016  # "$0" is expanded by the container's bash, on purpose
timeout --signal=INT --kill-after=20 "$TIMEOUT" docker run --rm --init --name "$NAME" --user "$(id -u):$(id -g)" \
  -e HOME=/home/u -e XDG_CONFIG_HOME=/home/u/.config -e XDG_DATA_HOME=/home/u/.local/share \
  -e XDG_RUNTIME_DIR=/tmp/run \
  -v "$BIN:/opt/chatty/chatty-tui:ro" -v "$HOME_DIR:/home/u" -v "$WORK_DIR:/work" -w /work \
  "$IMAGE" bash -c 'mkdir -p /tmp/run && exec /opt/chatty/chatty-tui --headless --team coder-reviewer --auto-approve --workspace /work -m "$0"' "$TASK" \
  > "$RUN_DIR/leader.out" 2> "$RUN_DIR/leader.err" &
leader_pid=$!
while kill -0 "$leader_pid" 2>/dev/null; do
  docker top "$NAME" -o pid,args 2>/dev/null \
    | awk '{for (i = 1; i <= NF; i++) if ($i == "--participant-name") print $1, $(i + 1)}' >> "$RUN_DIR/ps.log"
  sleep 3
done
wait "$leader_pid"
leader_exit=$?
set -e
wall=$(( $(date +%s) - start ))
cleanup

# 6. The verifier, in a second container.
set +e
docker run --rm --user "$(id -u):$(id -g)" -v "$WORK_DIR:/work" -v "$SMOKE/verify.sh:/verify.sh:ro" \
  "$IMAGE" bash /verify.sh /work > "$RUN_DIR/verify.log" 2>&1
verify_exit=$?
set -e
reward=$(sed -n 's/^REWARD=//p' "$RUN_DIR/verify.log" | tail -1)
reward="${reward:-0}"

# 7. One-screen summary.
{
  echo "== team smoke: $(basename "$RUN_DIR")"
  echo "leader exit=$leader_exit wall=${wall}s (timeout ${TIMEOUT}s)  base=$BASE_COMMIT"
  echo "team: $TEAM_DIR (the preset plus models and verification)"
  echo "processes spawned per role:"
  sort -u "$RUN_DIR/ps.log" \
    | awk '{sub(/-[0-9]+$/, "", $2); n[$2]++; c++} END {for (r in n) printf "  %s: %d\n", r, n[r]; if (!c) print "  (none seen)"}'
  echo "delegations (invoke_agent):"
  (grep -o '\[agent: [a-z-]*\] \[[a-z]*\] ⟳ running' "$RUN_DIR/leader.err" || true) \
    | awk '{sub(/\]$/, "", $2); n[$2]++; c++} END {for (a in n) printf "  %s: %d\n", a, n[a]; if (!c) print "  (none)"}'
  echo "reviewer's first line(s):"
  awk '/^  \[agent: local-reviewer\] ✓ completed/ {want = 2; next}
       want == 2 && /^    output/ {want = 1; next}
       want == 1 {sub(/^      /, ""); print "  " $0; want = 0}' "$RUN_DIR/leader.err"
  echo "merge commit: $(git -C "$WORK_DIR" log --merges --oneline -1 | grep . || echo '(none)')"
  echo "git log: $(git -C "$WORK_DIR" log --oneline --all | tr '\n' ';')"
  echo "leader's answer (first line): $(head -c 200 "$RUN_DIR/leader.out" | head -1)"
  echo "verifier (exit $verify_exit):"
  sed 's/^/  /' "$RUN_DIR/verify.log"
  echo "REWARD=$reward"
} | tee "$RUN_DIR/summary.txt"
[ "$reward" = "1" ]
