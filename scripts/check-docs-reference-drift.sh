#!/usr/bin/env bash
# AGE-268: the reference tables in scripts/gen-docs-reference.sh are
# hand-maintained. This check diffs them against the source so a new tool,
# stream event or settings field cannot land without a docs row.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
import re
import sys
from pathlib import Path

gen = Path("scripts/gen-docs-reference.sh").read_text()
failures = []

def section(text, start, end):
    i = text.index(start)
    j = text.index(end, i)
    return text[i:j]

# ── Tools: every `const NAME: &'static str = "…"` under chatty-core/src/tools ──
src_tools = set()
for path in Path("crates/chatty-core/src/tools").rglob("*.rs"):
    src_tools.update(re.findall(r'const NAME: &\'static str = "([a-z_]+)"', path.read_text()))
doc_tools = set(re.findall(r'^\s*\("([a-z_]+)",', section(gen, "tools = [", "]\nfor name"), re.M))
missing = sorted(src_tools - doc_tools)
extra = sorted(doc_tools - src_tools)
if missing:
    failures.append(f"tools-catalog: missing rows for {', '.join(missing)}")
if extra:
    failures.append(f"tools-catalog: rows for tools that no longer exist: {', '.join(extra)}")

# ── StreamManagerEvent variants ──
sm = Path("crates/chatty-gpui/src/chatty/models/stream_manager.rs").read_text()
enum_body = section(sm, "pub enum StreamManagerEvent", "\n}")
src_events = set(re.findall(r"^\s{4}([A-Z][A-Za-z]+)\b", enum_body, re.M))
catalog = section(gen, "# GPUI event catalog", "EOF")
doc_events = set(re.findall(r"`(?:StreamManagerEvent`\s*\|\s*`)?([A-Z][A-Za-z]+)`\s*\|", catalog))
missing = sorted(v for v in src_events if f"`{v}`" not in catalog)
if missing:
    failures.append(f"event-catalog: StreamManagerEvent variants without a row: {', '.join(missing)}")

# ── Every EventEmitter enum name should appear in the event catalog ──
emitters = set()
for path in Path("crates/chatty-gpui/src").rglob("*.rs"):
    emitters.update(re.findall(r"impl EventEmitter<([A-Za-z]+)> for", path.read_text()))
missing = sorted(e for e in emitters if f"`{e}`" not in catalog)
if missing:
    failures.append(f"event-catalog: event enums without a row: {', '.join(missing)}")

# ── Settings fields: ExecutionSettingsModel and ModelConfig ──
def struct_fields(path, name):
    text = Path(path).read_text()
    body = section(text, f"pub struct {name}", "\n}")
    return set(re.findall(r"^\s+pub ([a-z_]+):", body, re.M))

schema = section(gen, "# Settings schema reference", "EOF")
for path, name, heading in [
    ("crates/chatty-core/src/settings/models/execution_settings.rs", "ExecutionSettingsModel", "— `ExecutionSettingsModel`"),
    ("crates/chatty-core/src/settings/models/models_store.rs", "ModelConfig", "— `[ModelConfig]`"),
]:
    fields = struct_fields(path, name)
    table = section(schema, heading, "\n---")
    missing = sorted(f for f in fields if f"`{f}`" not in table)
    if missing:
        failures.append(f"settings-schema: {name} fields without a row: {', '.join(missing)}")

# ── Singletons: every OnceLock/LazyLock static that is not a regex/font cache ──
singletons = set()
for path in Path("crates/chatty-core/src").rglob("*.rs"):
    for name in re.findall(r"static ([A-Z_]+): (?:OnceLock|LazyLock)", path.read_text()):
        if name.startswith("RE_") or name in {"FONT_DB"}:
            continue  # immutable process caches, documented as a class
        singletons.add(name)
inventory = section(gen, "# Process-global singleton inventory", "EOF")
missing = sorted(s for s in singletons if f"`{s}`" not in inventory)
if missing:
    failures.append(f"singleton-inventory: statics without a row: {', '.join(missing)}")

if failures:
    for f in failures:
        print("reference drift:", f)
    print("  → update the table in scripts/gen-docs-reference.sh")
    sys.exit(1)
print("reference drift check: OK")
PY
