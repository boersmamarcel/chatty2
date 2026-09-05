#!/usr/bin/env bash
# Record a README / docs animation of the Chatty desktop app.
#
# Runs the real `chatty` binary inside a virtual X display (Xvfb + software
# Vulkan), points it at a scripted stand-in for Ollama (mock_ollama.py), drives
# it with xdotool from a scenario's steps.sh, captures the window with ffmpeg
# and converts the result to a GIF. Nothing touches your own Chatty profile:
# every run gets a throw-away HOME.
#
# Usage:
#   scripts/animations/record.sh [options] <scenario> [<scenario>...]
#   scripts/animations/record.sh --all
#
# Options:
#   --app PATH       chatty binary to run (default: $CHATTY_BIN, then
#                    target/release/chatty, then target/debug/chatty)
#   --out DIR        where GIFs go (default: assets/animations)
#   --work DIR       scratch dir for videos, logs and profiles
#                    (default: target/animations)
#   --keep-mp4       also keep the lossless-ish MP4 next to the GIF
#   --no-gif         stop after the MP4 (useful while tuning a scenario)
#
# A scenario is a directory under scripts/animations/scenarios/<name>/ with:
#   scenario.json   replies for mock_ollama.py (see that file's docstring)
#   steps.sh        the interaction, written with the helpers below
#   workspace/      optional files copied into the agent workspace
#   profile/        optional JSON files overriding scripts/animations/profile/
#   settings.sh     optional overrides: WIDTH, HEIGHT, SCALE, GIF_WIDTH, FPS
#   setup.sh        optional: runs before the app starts, with $RUN_DIR set.
#                   Use it to prepare the workspace (e.g. `git init`) or to
#                   drop a stub command in $RUN_DIR/bin, which is first on
#                   the app's PATH.
#
# Requirements (Debian/Ubuntu): xvfb openbox xdotool ffmpeg imagemagick x11-apps
# mesa-vulkan-drivers python3. See scripts/animations/README.md.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

APP="${CHATTY_BIN:-}"
OUT_DIR="$ROOT/assets/animations"
WORK_DIR="$ROOT/target/animations"
KEEP_MP4=0
NO_GIF=0
ALL=0
SCENARIOS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --work) WORK_DIR="$2"; shift 2 ;;
    --keep-mp4) KEEP_MP4=1; shift ;;
    --no-gif) NO_GIF=1; shift ;;
    --all) ALL=1; shift ;;
    -h|--help) sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) SCENARIOS+=("$1"); shift ;;
  esac
done

if [[ $ALL -eq 1 ]]; then
  for d in "$HERE"/scenarios/*/; do SCENARIOS+=("$(basename "$d")"); done
fi
if [[ ${#SCENARIOS[@]} -eq 0 ]]; then
  echo "no scenario given; try --all or one of:" >&2
  ls "$HERE/scenarios" >&2
  exit 2
fi

if [[ -z "$APP" ]]; then
  for candidate in "$ROOT/target/release/chatty" "$ROOT/target/debug/chatty"; do
    [[ -x "$candidate" ]] && APP="$candidate" && break
  done
fi
if [[ -z "$APP" || ! -x "$APP" ]]; then
  echo "no chatty binary found. Build one (cargo build --release -p chatty-gpui)," >&2
  echo "extract a release AppImage (./chatty-linux-x86_64.AppImage --appimage-extract)" >&2
  echo "and pass --app squashfs-root/usr/bin/chatty, or set CHATTY_BIN." >&2
  exit 1
fi
APP="$(readlink -f "$APP")"

for tool in Xvfb openbox xdotool ffmpeg python3 xwd convert; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
done

# An extracted AppImage keeps its themes and pdfium next to the binary.
APP_ROOT="$(cd "$(dirname "$APP")/.." && pwd)"
export CHATTY_DATA_DIR="${CHATTY_DATA_DIR:-$APP_ROOT/share/chatty}"
[[ -d "$CHATTY_DATA_DIR" ]] || export CHATTY_DATA_DIR="$ROOT"   # cargo build: ./themes
export LD_LIBRARY_PATH="$APP_ROOT/lib:${LD_LIBRARY_PATH:-}"

# Software Vulkan (lavapipe) so this works on a headless box; a real GPU is
# used automatically when the lavapipe ICD file is absent.
LVP_ICD=/usr/share/vulkan/icd.d/lvp_icd.json
if [[ -z "${VK_ICD_FILENAMES:-}" && -f "$LVP_ICD" && -z "${CHATTY_RECORD_USE_GPU:-}" ]]; then
  export VK_ICD_FILENAMES="$LVP_ICD" VK_DRIVER_FILES="$LVP_ICD"
fi

mkdir -p "$OUT_DIR" "$WORK_DIR"

PIDS=()
cleanup() {
  set +e
  for pid in "${PIDS[@]:-}"; do [[ -n "$pid" ]] && kill "$pid" 2>/dev/null; done
  sleep 0.5
  for pid in "${PIDS[@]:-}"; do [[ -n "$pid" ]] && kill -9 "$pid" 2>/dev/null; done
  PIDS=()
}
trap cleanup EXIT INT TERM

free_display() {
  local n=90
  while [[ -e "/tmp/.X11-unix/X$n" ]]; do n=$((n + 1)); done
  echo "$n"
}

# ── step helpers, available to steps.sh ─────────────────────────────────────
# Coordinates are in logical points relative to the app window; SCALE and the
# window origin are applied here.
_px() { python3 -c "import sys; print(int(round(float(sys.argv[1]) * float(sys.argv[2]) + float(sys.argv[3]))))" "$1" "$SCALE" "$2"; }
pause() { sleep "$1"; }
move_to() { xdotool mousemove "$(_px "$1" "$WIN_X")" "$(_px "$2" "$WIN_Y")"; }
click() { move_to "$1" "$2"; sleep 0.15; xdotool click 1; }
press() { xdotool key --delay 60 "$@"; }
type_text() { xdotool type --delay "${TYPE_DELAY_MS:-38}" -- "$1"; }
# Click the composer, type a prompt like a person would, and send it.
say() {
  click "$(( LOGICAL_W / 2 + 120 ))" "$(( LOGICAL_H - 60 ))"
  sleep 0.4
  type_text "$1"
  sleep 0.7
  press Return
}
# Block until the scripted model has finished a text reply (tool-call turns
# in between do not count), then settle so the final frame is painted.
wait_reply() {
  local want=$(( $(grep -c "turn complete" "$MOCK_LOG" 2>/dev/null || true) + 1 ))
  local deadline=$(( SECONDS + ${1:-90} ))
  while (( $(grep -c "turn complete" "$MOCK_LOG" 2>/dev/null || true) < want )); do
    (( SECONDS > deadline )) && { echo "wait_reply: timed out" >&2; return 1; }
    sleep 0.25
  done
  sleep "${2:-1.5}"
}
collapse_sidebar() { press ctrl+b; sleep 0.6; }
screenshot() { xwd -silent -id "$WIN" | convert xwd:- "$RUN_DIR/$1.png"; }
# ─────────────────────────────────────────────────────────────────────────────

record_one() {
  local name="$1"
  local scen="$HERE/scenarios/$name"
  [[ -f "$scen/scenario.json" && -f "$scen/steps.sh" ]] || { echo "not a scenario: $scen" >&2; return 1; }

  # Per-scenario knobs (physical window size, GPUI scale, GIF width, fps).
  WIDTH=1600; HEIGHT=1000; SCALE=1.5; GIF_WIDTH=1200; FPS=15
  [[ -f "$scen/settings.sh" ]] && source "$scen/settings.sh"
  LOGICAL_W=$(python3 -c "print(int($WIDTH / $SCALE))")
  LOGICAL_H=$(python3 -c "print(int($HEIGHT / $SCALE))")

  RUN_DIR="$WORK_DIR/$name"
  rm -rf "$RUN_DIR"; mkdir -p "$RUN_DIR/home/.config/chatty" "$RUN_DIR/home/.local/share/chatty" "$RUN_DIR/workspace"
  [[ -d "$scen/workspace" ]] && cp -r "$scen/workspace/." "$RUN_DIR/workspace/"

  local display; display="$(free_display)"
  local port=$(( 11500 + display ))
  local proxy_port=$(( 12500 + display ))

  # Profile: shared templates, overridable per scenario.
  for f in "$HERE"/profile/*.json; do
    local base; base="$(basename "$f")"
    local src="$f"; [[ -f "$scen/profile/$base" ]] && src="$scen/profile/$base"
    sed -e "s#@PORT@#$port#g" -e "s#@WORKSPACE@#$RUN_DIR/workspace#g" "$src" > "$RUN_DIR/home/.config/chatty/$base"
  done

  # Anything the scenario needs in place before the app starts: a prepared
  # workspace, or a stub command in $RUN_DIR/bin (first on the app's PATH).
  mkdir -p "$RUN_DIR/bin"
  [[ -f "$scen/setup.sh" ]] && ( set -e; source "$scen/setup.sh" )

  MOCK_LOG="$RUN_DIR/mock.log"
  python3 "$HERE/mock_ollama.py" --port "$port" --scenario "$scen/scenario.json" 2> "$MOCK_LOG" &
  PIDS+=($!)
  # The update check would otherwise report "Update failed" in the footer on
  # an offline machine; park it on a proxy that never answers.
  python3 - "$proxy_port" <<'PY' &
import socket, sys, threading, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(64)
def hold(c):
    try: time.sleep(3600)
    finally: c.close()
while True:
    c, _ = s.accept(); threading.Thread(target=hold, args=(c,), daemon=True).start()
PY
  PIDS+=($!)

  Xvfb ":$display" -screen 0 "$(( WIDTH + 100 ))x$(( HEIGHT + 100 ))x24" -nolisten tcp >/dev/null 2>&1 &
  PIDS+=($!)
  sleep 1
  export DISPLAY=":$display"
  openbox >/dev/null 2>&1 &
  PIDS+=($!)
  sleep 0.5

  local run_home="$RUN_DIR/home"
  mkdir -p "$RUN_DIR/xdg-runtime"; chmod 700 "$RUN_DIR/xdg-runtime"
  (
    export HOME="$run_home" XDG_CONFIG_HOME="$run_home/.config" XDG_DATA_HOME="$run_home/.local/share" \
      XDG_RUNTIME_DIR="$RUN_DIR/xdg-runtime" LC_ALL=C.UTF-8 WAYLAND_DISPLAY= \
      GPUI_X11_SCALE_FACTOR="$SCALE" RUST_LOG="${RUST_LOG:-warn}" \
      PATH="$RUN_DIR/bin:$PATH" \
      HTTPS_PROXY="http://127.0.0.1:$proxy_port" HTTP_PROXY="http://127.0.0.1:$proxy_port" \
      https_proxy="http://127.0.0.1:$proxy_port" http_proxy="http://127.0.0.1:$proxy_port" \
      NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost
    exec "$APP" > "$RUN_DIR/app.log" 2>&1
  ) &
  local app_pid=$!
  PIDS+=($app_pid)

  WIN=""
  for _ in $(seq 1 60); do
    WIN="$(xdotool search --onlyvisible --name '^Chatty$' 2>/dev/null | head -1 || true)"
    [[ -n "$WIN" ]] && break
    sleep 0.5
  done
  [[ -n "$WIN" ]] || { echo "chatty window never appeared; see $RUN_DIR/app.log" >&2; return 1; }
  xdotool windowmove "$WIN" 0 0 windowsize "$WIN" "$WIDTH" "$HEIGHT"
  sleep 2.5
  eval "$(xdotool getwindowgeometry --shell "$WIN")"
  WIN_X=$X; WIN_Y=$Y

  local mp4="$RUN_DIR/$name.mp4"
  ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 -framerate 30 -video_size "${WIDTH}x${HEIGHT}" \
    -i ":$display+$WIN_X,$WIN_Y" -c:v libx264 -preset ultrafast -qp 0 -pix_fmt yuv444p "$mp4" &
  local ff_pid=$!
  PIDS+=($ff_pid)
  sleep 1

  echo "==> $name: recording on :$display (window ${WIDTH}x${HEIGHT} @ ${SCALE}x)"
  local ok=0
  ( set -e; source "$scen/steps.sh" ) || ok=$?
  sleep 0.5
  xwd -silent -id "$WIN" | convert xwd:- "$RUN_DIR/final.png"
  kill -INT "$ff_pid" 2>/dev/null || true
  wait "$ff_pid" 2>/dev/null || true
  kill "$app_pid" 2>/dev/null || true
  cleanup
  [[ $ok -eq 0 ]] || { echo "steps.sh failed ($ok); logs in $RUN_DIR" >&2; return "$ok"; }

  if [[ $NO_GIF -eq 1 ]]; then
    echo "    video: $mp4"
    return 0
  fi
  local gif="$OUT_DIR/$name.gif"
  # The window manager can leave a black strip inside the captured region;
  # crop to the app's own pixels (bounding box of the non-black area).
  local crop
  crop="$(convert "$RUN_DIR/final.png" -fuzz 2% -trim -format '%w:%h:%X:%Y' info: 2>/dev/null | tr -d '+')"
  [[ -n "$crop" ]] || crop="iw:ih:0:0"
  ffmpeg -loglevel error -y -i "$mp4" -filter_complex \
    "[0:v]crop=$crop,fps=$FPS,scale=$GIF_WIDTH:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=200:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
    "$gif"
  [[ $KEEP_MP4 -eq 1 ]] && cp "$mp4" "$OUT_DIR/$name.mp4"
  echo "    gif:   $gif ($(du -h "$gif" | cut -f1))"
}

status=0
for name in "${SCENARIOS[@]}"; do
  record_one "$name" || status=1
done
exit $status
