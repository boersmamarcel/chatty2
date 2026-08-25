#!/usr/bin/env python3
"""Mechanically migrate chatty Tool impls from rig 0.37 → rig-agent 0.42."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / "crates/chatty-core/src/tools"


def find_matching_brace(s: str, open_idx: int) -> int:
    """Return index of closing brace matching s[open_idx] == '{'."""
    depth = 0
    i = open_idx
    while i < len(s):
        c = s[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        elif c == '"':
            i += 1
            while i < len(s):
                if s[i] == "\\":
                    i += 2
                    continue
                if s[i] == '"':
                    break
                i += 1
        elif c == "'":
            i += 1
            while i < len(s) and s[i] != "'":
                if s[i] == "\\":
                    i += 2
                    continue
                i += 1
        i += 1
    raise ValueError("unbalanced braces")


def migrate_definition_block(src: str) -> str:
    """Replace definition() methods that return ToolDefinition { name, description, parameters }."""
    sig = re.compile(
        r"async fn definition\(&self,\s*_?prompt:\s*String\)\s*->\s*ToolDefinition\s*\{"
    )
    out = []
    pos = 0
    for m in sig.finditer(src):
        out.append(src[pos : m.start()])
        # body starts at m.end()-1 which is the opening '{' of the fn body
        fn_open = m.end() - 1
        fn_close = find_matching_brace(src, fn_open)
        body = src[fn_open + 1 : fn_close]

        # Find ToolDefinition { ... } inside body
        td = re.search(r"ToolDefinition\s*\{", body)
        if not td:
            # leave unchanged
            out.append(src[m.start() : fn_close + 1])
            pos = fn_close + 1
            continue
        td_open = td.end() - 1
        td_close = find_matching_brace(body, td_open)
        fields = body[td_open + 1 : td_close]

        # Parse top-level fields: name, description, parameters
        # Walk fields splitting on commas at depth 0
        parts: list[str] = []
        depth = 0
        start = 0
        i = 0
        while i < len(fields):
            c = fields[i]
            if c in "{[(":
                depth += 1
            elif c in "}])":
                depth -= 1
            elif c == '"' and depth == 0:
                i += 1
                while i < len(fields):
                    if fields[i] == "\\":
                        i += 2
                        continue
                    if fields[i] == '"':
                        break
                    i += 1
            elif c == "," and depth == 0:
                parts.append(fields[start:i].strip())
                start = i + 1
            i += 1
        tail = fields[start:].strip()
        if tail:
            parts.append(tail)

        field_map: dict[str, str] = {}
        for part in parts:
            if not part:
                continue
            km = re.match(r"(\w+)\s*:", part)
            if not km:
                continue
            key = km.group(1)
            val = part[km.end() :].strip().rstrip(",")
            field_map[key] = val

        if "description" not in field_map or "parameters" not in field_map:
            out.append(src[m.start() : fn_close + 1])
            pos = fn_close + 1
            continue

        desc = field_map["description"]
        params = field_map["parameters"]
        replacement = (
            f"fn description(&self) -> String {{\n"
            f"        {desc}\n"
            f"    }}\n"
            f"\n"
            f"    fn parameters(&self) -> serde_json::Value {{\n"
            f"        {params}\n"
            f"    }}"
        )
        out.append(replacement)
        pos = fn_close + 1
    out.append(src[pos:])
    return "".join(out)


def migrate_call_sigs(src: str) -> str:
    src = re.sub(
        r"async fn call\(&self,\s*((?:_|args|arg)[^)]*)\)",
        r"async fn call(&self, _context: &mut ToolContext, \1)",
        src,
    )
    # Guard against double insertion
    while "_context: &mut ToolContext, _context: &mut ToolContext," in src:
        src = src.replace(
            "_context: &mut ToolContext, _context: &mut ToolContext,",
            "_context: &mut ToolContext,",
        )
    return src


def migrate_imports(src: str) -> str:
    src = src.replace(
        "use rig_core::tool::Tool;",
        "use rig_agent::tool::{Tool, ToolContext};",
    )
    src = src.replace(
        "use rig_core::tool::Tool as RigTool;",
        "use rig_agent::tool::{tool_definition, Tool as RigTool, ToolContext};",
    )
    return src


def migrate_test_calls(src: str) -> str:
    src = re.sub(
        r"(\w+)\.definition\([^)]*\)\.await",
        r"rig_agent::tool::tool_definition(&\1)",
        src,
    )

    def call_repl(m: re.Match[str]) -> str:
        prefix = m.group(1)
        args = m.group(2)
        if "ToolContext" in args:
            return m.group(0)
        return f"{prefix}.call(&mut ToolContext::new(), {args}).await"

    src = re.sub(r"([.\w]+)\.call\(([^)]*)\)\.await", call_repl, src)
    return src


def ensure_test_imports(src: str) -> str:
    if "ToolContext::new()" not in src:
        return src
    if "use rig_agent::tool::{Tool, ToolContext}" in src:
        return src
    if "use rig_agent::tool::ToolContext" in src:
        return src
    if "#[cfg(test)]\nmod tests {" in src:
        src = src.replace(
            "#[cfg(test)]\nmod tests {",
            "#[cfg(test)]\nmod tests {\n    use rig_agent::tool::ToolContext;\n",
            1,
        )
    return src


def drop_unused_tooldefinition_import(src: str) -> str:
    if "use rig_core::completion::ToolDefinition;" not in src:
        return src
    without = src.replace("use rig_core::completion::ToolDefinition;\n", "").replace(
        "use rig_core::completion::ToolDefinition;", ""
    )
    if "ToolDefinition" not in without:
        return without
    return src


def process_file(path: Path) -> bool:
    original = path.read_text()
    src = original
    src = migrate_definition_block(src)
    src = migrate_call_sigs(src)
    src = migrate_imports(src)
    src = migrate_test_calls(src)
    src = ensure_test_imports(src)
    src = drop_unused_tooldefinition_import(src)
    if src != original:
        path.write_text(src)
        return True
    return False


def main() -> int:
    changed = []
    for path in sorted(TOOLS.rglob("*.rs")):
        if process_file(path):
            changed.append(str(path.relative_to(ROOT)))
            print(f"migrated {path.relative_to(ROOT)}")
    print(f"\n{len(changed)} files changed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
