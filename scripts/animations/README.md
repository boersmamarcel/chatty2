# Recording the README / docs animations

The GIFs in `assets/animations/` are recorded, not hand-made. Re-run the
recorder whenever the UI changes enough that they look dated:

```bash
sudo apt-get install -y xvfb openbox xdotool ffmpeg imagemagick x11-apps mesa-vulkan-drivers
scripts/animations/record.sh --all            # or: make animations
scripts/animations/record.sh hero mermaid     # just some of them
```

`record.sh` starts the real desktop app inside a virtual X display, drives it
with `xdotool`, captures the window with `ffmpeg` and writes
`assets/animations/<scenario>.gif`. Each run gets a throw-away `HOME`, so your
own Chatty profile is never touched. The MP4, logs and the profile it used are
left under `target/animations/<scenario>/` for inspection (`final.png` there is
the last frame).

## Which binary

Pass `--app` (or set `CHATTY_BIN`). Without it the script looks for
`target/release/chatty`, then `target/debug/chatty`. The quickest way to record
against a shipped build is the Linux AppImage from GitHub Releases:

```bash
./chatty-linux-x86_64.AppImage --appimage-extract
scripts/animations/record.sh --app squashfs-root/usr/bin/chatty --all
```

Rendering uses Mesa's software Vulkan driver (lavapipe) when its ICD file is
present, so this works on a headless box or in CI. Set
`CHATTY_RECORD_USE_GPU=1` to use the machine's own GPU instead.

## No API keys needed

The app talks to `mock_ollama.py`, a tiny stand-in for an Ollama server that
streams scripted replies (text and tool calls) from the scenario's
`scenario.json`. Everything else is real: the transcript, tool execution in
the workspace, the plan strip, diffs, token tracking and title generation.

The mock advertises no models on `/api/tags`, so Chatty's Ollama sync leaves
the seeded (user-owned) `llama3.2` entry alone; set `"list_model": true` in a
scenario to exercise the sync path instead. The update check is parked on a
local proxy that never answers, so the footer shows "Checking..." rather than
"Update failed" on an offline machine.

## Anatomy of a scenario

```
scripts/animations/scenarios/<name>/
├── scenario.json   replies for mock_ollama.py (docstring in that file)
├── steps.sh        the interaction, using the helpers below
├── settings.sh     optional: WIDTH, HEIGHT, SCALE, GIF_WIDTH, FPS
├── workspace/      optional files copied into the agent workspace
└── profile/        optional overrides for scripts/animations/profile/*.json
```

`steps.sh` runs inside `record.sh`, so it can use:

| Helper | What it does |
|:-------|:-------------|
| `say "prompt"` | click the composer, type the prompt, press Enter |
| `wait_reply [timeout] [settle]` | block until the mock finished a text reply |
| `pause N` | sleep |
| `click X Y` / `move_to X Y` | pointer actions in logical (unscaled) window coordinates |
| `press KEYS` | `xdotool key`, e.g. `press ctrl+b` |
| `collapse_sidebar` | Ctrl+B |
| `screenshot NAME` | save `target/animations/<scenario>/NAME.png` |

Coordinates are logical points; `record.sh` multiplies by `SCALE`
(`GPUI_X11_SCALE_FACTOR`, 1.5 by default so text stays legible once the GIF is
shrunk to README width).

The transcript stays pinned to the bottom while a reply streams (wheel events do
not reach the app under Xvfb), so size replies to fit the window.

Reply text is streamed word by word. Replies that mix prose and fenced code
should put the code block last: with the current renderer a second prose
segment after a code block shows the wrong text (see
`render_cached_markdown_segments` in
`crates/chatty-gpui/src/chatty/views/message_component.rs`).

Some gpui-component controls (tab bars, popover triggers) ignore the
synthetic clicks xdotool sends under Xvfb; buttons, cards and the composer
work. Prefer flows that open a panel by clicking a card or that auto-open
(PDF, chart and query-table artifacts do).

## Keeping the docs in sync

`README.md` embeds `hero.gif`; `docs-site/src/user/*.md` embed the smaller
feature GIFs, which `make docs-sync` copies into the book (see the list in
`scripts/docs-sync.sh`). Keep new docs GIFs under about 3 MB, or link to them
instead of embedding.
