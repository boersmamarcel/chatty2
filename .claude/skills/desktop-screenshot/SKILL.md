---
name: desktop-screenshot
description: Run the Chatty desktop app (chatty-gpui) headless under Xvfb, drive it with synthetic mouse/keyboard input, and take screenshots to check a UI change for real. Use whenever a desktop UI change needs visual proof (artifact panel, file explorer, browser panel, dialogs, menus, drag-and-drop), when asked to "run", "launch", "screenshot" or "verify in the app", or when a unit test cannot see the behaviour (layout, focus, gpui event routing).
allowed-tools: Bash, Read, Write, Edit, Grep, Glob
---

# Desktop screenshot harness

Unit tests do not see gpui layout, focus, or event routing. The last three
desktop features each had defects that *only* a running app showed (a list
with zero height, a focus stolen by a menu, a save writing to `…/file/`).
This skill is the way to see them before the PR does.

## Scripts (in `scripts/`)

| Script | What |
|---|---|
| `start.sh` | Xvfb + the app on an isolated `XDG_CONFIG_HOME`, resized and focused. Prints the pid and the next command. |
| `x.py` | XTest driver: `geom shot crop click rclick dblclick ctrlclick shiftclick drag wheel type key ctrl/alt/shift`. Run `x.py` with no args for the list. |
| `stop.sh` | Kills the app, leaked headless Chromes, and Xvfb. |

## Recipe

```bash
# 1. Build (reuse a sibling worktree's target/ if you have one: it saves 40 min)
cargo build -p chatty-gpui            # or CARGO_TARGET_DIR=… cargo build -p chatty-gpui

# 2. Start on a virtual display with a scratch workspace to click around in
S=$PWD/.claude/skills/desktop-screenshot/scripts     # absolute: you cd away next
$S/start.sh --dir /tmp/shot-run --workspace /tmp/shot-run/ws   # seed ws/ with files first
export DISPLAY=:99; cd /tmp/shot-run

# 3. Drive and shoot
python3 $S/x.py shot shots/01-start.png
python3 $S/x.py click 1587 17          # the artifact picker caret (1600px wide window)
python3 $S/x.py click 1500 50          # "Files" → the explorer
python3 $S/x.py shot shots/02.png
python3 $S/x.py crop shots/02.png shots/02-crop.png 960 36 1600 400   # then Read it

# 4. Stop; never leave the app running (it holds the display and leaks Chromes)
$S/stop.sh
```

Then **Read the PNG** (the Read tool renders images) and say what you see.
Crop to the region that matters — a 1600×1000 frame reads badly, a 640×360
crop reads well. Put the final shots together with PIL into one or two
contact sheets and attach them to the Linear issue
(`prepare_attachment_upload` → `curl -X PUT` → `create_attachment_from_upload`);
`gh` cannot upload images to a PR.

`python3` must be one with `python-xlib` and `Pillow` — on Marcel's
workstation that is the pyenv shim (3.6), **not** `/usr/bin/python3`.
`pip install --user python-xlib pillow` elsewhere.

## Getting the UI into the state you want

- **Artifact panel without a model call**: titlebar caret → *Files* opens the
  explorer; click a file in the tree to open it through every viewer
  (markdown, code, PDF, PPTX, CSV, image). Put the files you need in the
  workspace before `start.sh`. Caret → *Browser* starts a browser session.
- **A tool-card artifact** (what the agent would have produced): seed a
  conversation row whose `system_traces` carries a finished `ToolCall`
  (see the AGE-472 recipe in the memory notes) — only needed when the card
  itself is under test.
- **A real model turn**: point `config/chatty/providers.json` at a local
  OpenAI-compatible server (vLLM/Ollama; the OpenRouter provider type
  honours `base_url`). Ask for exactly one tool call and "reply done": a
  prompt that triggers the todo protocol rebuilds the agent mid-turn.
- **Coordinates move.** Opening the first file adds the tab bar (+28 px),
  the header buttons shift when Copy/Save appear, dialogs vary in height.
  Take a shot and read it before clicking into a new state; do not chain
  blind clicks.

## Traps (each one cost an hour once)

- **Never drive the real display** (`:0`, `:1`). XTest on `:1` once typed a
  prompt into Marcel's browser. `start.sh` defaults to `:99`.
- **A click needs a MotionNotify first.** gpui ignores an XTest button press
  without a preceding pointer motion; `warp_pointer` alone does nothing.
  `x.py` does two motions before every press — keep that if you extend it.
- **No window manager**: `set_input_focus` (`x.py focus`) before typing, or
  keys go nowhere. Popup menus and dialogs *do* open under XTest.
- **Drags** register at ~0.35 s per step on Xvfb (`x.py drag … 8`). On the
  real display with no monitor attached (`xrandr` all disconnected) every
  present blocks ~1 s and each step must hold ≥1.5 s.
- **Escape closes the artifact panel** (its `on_key_down`). An inner element
  that wants Escape must `stop_propagation()` first.
- **`pkill -f <pattern>` inside a compound command kills the command itself**
  when the pattern's text appears anywhere in that command line, even with
  the `[c]` trick. Run `pkill` alone or use `stop.sh`.
- **Killing the app leaks headless Chromes** (`--user-data-dir=/tmp/chatty-browser-*`);
  `stop.sh` sweeps them.
- **The user's real data**: `XDG_CONFIG_HOME` must never be `~/.config`.
  `start.sh` writes its own `config/`.
- **Other work on the machine**: Harbor eval jobs run `chatty-tui` processes
  here; match on `debug/chatty`, never on a bare `chatty`.
- **Which binary is that?** `start.sh` runs `$CARGO_TARGET_DIR/debug/chatty`
  (or `--bin`). A `target/` shared between worktrees holds whichever session
  built last — check the version in the footer of the first screenshot and
  the UI you expect before trusting a run; pass `--bin` to be explicit.
- **Rendering**: `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`
  (lavapipe) renders gpui correctly on Xvfb; `start.sh` sets it. Frame times:
  `ZED_MEASUREMENTS=1` prints `frame duration` per frame if speed is the question.

## gpui facts these runs established

- `div()` is a **block** container. A `uniform_list(...).flex_1()` in a plain
  `div()` gets zero height and renders nothing — wrap it in `v_flex()`.
- `on_key_down` on the panel sees keys typed in a focused child `Input`
  (that is how Escape-to-close and Ctrl+S work).
- A `PopupMenu` item handler runs *before* the menu dismisses; focus set in
  the handler can be undone. Set a flag and focus from the next render.
- A row's `on_drop` stops propagation, so a nested drop target (the list
  body) never double-fires.
- `ResizableState` has no public size setter; re-create the entity to change
  a docked panel's default width.
- `presentation_on_open` docks a full-window panel; user navigation inside
  the panel must save and restore `mode` around `open()`.
