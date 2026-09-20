#!/usr/bin/env bash
# Start Xvfb and the Chatty desktop app against an isolated config, for
# screenshot-driven checks. Prints the app pid. Idempotent per display.
#
#   start.sh [--display :99] [--size 1600x1000] [--dir /tmp/shot-run]
#            [--workspace /path/to/ws] [--bin target/debug/chatty]
#
# --dir      scratch dir: config/ (XDG_CONFIG_HOME), shots/, app.log
# --workspace  written into config/chatty/execution_settings.json as
#            workspace_dir (what the file explorer and the tools root at);
#            defaults to <dir>/ws, created if missing
# --bin      the binary to run (default: a debug build in this checkout,
#            honouring CARGO_TARGET_DIR)
set -euo pipefail

DISPLAY_NO=":99"; SIZE="1600x1000"; DIR="/tmp/chatty-shot"; WS=""; BIN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --display) DISPLAY_NO="$2"; shift 2;;
    --size) SIZE="$2"; shift 2;;
    --dir) DIR="$2"; shift 2;;
    --workspace) WS="$2"; shift 2;;
    --bin) BIN="$2"; shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
BIN="${BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/chatty}"
[ -x "$BIN" ] || { echo "no binary at $BIN — cargo build -p chatty-gpui first" >&2; exit 1; }
WS="${WS:-$DIR/ws}"
mkdir -p "$DIR/config/chatty" "$DIR/shots" "$WS"

# Only the settings the UI needs; everything else takes its defaults. Never
# point XDG_CONFIG_HOME at ~/.config — that is the user's real database.
if [ ! -f "$DIR/config/chatty/execution_settings.json" ]; then
  cat > "$DIR/config/chatty/execution_settings.json" <<JSON
{
  "enabled": true,
  "approval_mode": "AlwaysAsk",
  "workspace_dir": "$WS",
  "filesystem_read_enabled": true,
  "filesystem_write_enabled": true,
  "fetch_enabled": false,
  "git_enabled": false,
  "browser_enabled": false,
  "execute_code_enabled": false,
  "docker_code_execution_enabled": false,
  "timeout_seconds": 30,
  "max_output_bytes": 1048576,
  "network_isolation": false,
  "max_agent_turns": 50,
  "memory_enabled": false,
  "embedding_enabled": false,
  "hosted_conversations_enabled": false
}
JSON
fi

if ! pgrep -f "Xvfb $DISPLAY_NO" >/dev/null; then
  Xvfb "$DISPLAY_NO" -screen 0 "${SIZE}x24" >"$DIR/xvfb.log" 2>&1 &
  sleep 1.5
fi

DISPLAY="$DISPLAY_NO" XDG_CONFIG_HOME="$DIR/config" \
  VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
  nohup "$BIN" >"$DIR/app.log" 2>&1 &
APP_PID=$!
sleep 10
W="${SIZE%x*}"; H="${SIZE#*x}"
X="$(cd "$(dirname "$0")" && pwd)/x.py"
DISPLAY="$DISPLAY_NO" python3 "$X" resize "$W" "$H" >/dev/null || true
sleep 1.5
DISPLAY="$DISPLAY_NO" python3 "$X" focus >/dev/null || true
echo "app pid $APP_PID on $DISPLAY_NO; binary $BIN; scratch $DIR; workspace $WS"
echo "export DISPLAY=$DISPLAY_NO X=$X; cd $DIR; python3 \$X shot shots/01.png"
