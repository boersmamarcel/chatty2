#!/usr/bin/env bash
# AGE-115: validate optional YAML frontmatter on markdown doc pages.
# Pages without a leading --- fence are skipped. Invalid fences fail the check.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - "$@" <<'PY'
from __future__ import annotations

import os
import sys
from pathlib import Path

ALLOWED_AUDIENCE = {"contributor", "agent", "user"}
ALLOWED_KEYS = {"audience", "source_files", "related"}
ROOT = Path(".").resolve()

SCAN_PATHS = [
    Path("docs"),
    Path("docs-site/src"),
    Path("AGENTS.md"),
    Path("CLAUDE.md"),
    Path("CONTRIBUTING.md"),
]


def iter_markdown() -> list[Path]:
    files: list[Path] = []
    for p in SCAN_PATHS:
        if not p.exists():
            continue
        if p.is_file():
            files.append(p)
            continue
        for f in p.rglob("*.md"):
            if "docs/generated" in f.as_posix():
                continue
            files.append(f)
    return sorted(files)


def parse_scalar_list(raw: str) -> list[str] | str:
    raw = raw.strip()
    if raw.startswith("[") and raw.endswith("]"):
        inner = raw[1:-1].strip()
        if not inner:
            return []
        return [part.strip().strip("'\"") for part in inner.split(",") if part.strip()]
    return raw.strip().strip("'\"")


def parse_frontmatter(text: str) -> dict[str, object] | None:
    if not text.startswith("---"):
        return None
    newline = "\n"
    if text.startswith("---\r\n"):
        rest = text[5:]
        newline = "\r\n"
    elif text.startswith("---\n"):
        rest = text[4:]
    else:
        raise ValueError("opening --- must be on its own line")

    closer = None
    for token in ("\n---\n", "\n---\r\n", "\n---"):
        idx = rest.find(token)
        if idx != -1:
            closer = (idx, token)
            break
    if closer is None:
        raise ValueError("unclosed frontmatter fence")
    body = rest[: closer[0]]
    return parse_yaml_subset(body)


def parse_yaml_subset(body: str) -> dict[str, object]:
    data: dict[str, object] = {}
    current_key: str | None = None
    current_list: list[str] | None = None

    def flush_list() -> None:
        nonlocal current_key, current_list
        if current_key is not None and current_list is not None:
            data[current_key] = current_list
        current_key = None
        current_list = None

    for raw_line in body.splitlines():
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        stripped = raw_line.lstrip()
        if stripped.startswith("- "):
            if current_list is None or current_key is None:
                raise ValueError(f"list item without a key: {raw_line!r}")
            current_list.append(stripped[2:].strip().strip("'\""))
            continue
        if ":" not in raw_line:
            raise ValueError(f"expected key: value, got {raw_line!r}")
        flush_list()
        key, _, val = stripped.partition(":")
        key = key.strip()
        val = val.strip()
        if not key:
            raise ValueError("empty key")
        if val == "":
            current_key = key
            current_list = []
            continue
        data[key] = parse_scalar_list(val)
        current_key = None
        current_list = None
    flush_list()
    return data


def validate(data: dict[str, object], path: Path) -> list[str]:
    errors: list[str] = []
    unknown = sorted(set(data) - ALLOWED_KEYS)
    if unknown:
        errors.append(f"{path}: unknown frontmatter keys: {', '.join(unknown)}")
    if "audience" not in data:
        errors.append(f"{path}: missing required key 'audience'")
    else:
        audience = data["audience"]
        if not isinstance(audience, list) or not audience:
            errors.append(f"{path}: audience must be a non-empty list")
        else:
            bad = [a for a in audience if a not in ALLOWED_AUDIENCE]
            if bad:
                errors.append(
                    f"{path}: audience values must be contributor|agent|user, got {bad}"
                )
    for key in ("source_files", "related"):
        if key in data and not isinstance(data[key], list):
            errors.append(f"{path}: {key} must be a list")
    return errors


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()

    errors: list[str] = []
    checked = 0
    skipped = 0
    for path in iter_markdown():
        text = path.read_text(encoding="utf-8")
        try:
            parsed = parse_frontmatter(text)
        except ValueError as exc:
            errors.append(f"{path}: {exc}")
            continue
        if parsed is None:
            skipped += 1
            continue
        checked += 1
        errors.extend(validate(parsed, path))

    if errors:
        print("frontmatter check failed:")
        for err in errors:
            print(f"  {err}")
        return 1
    print(f"frontmatter check: OK ({checked} pages with frontmatter, {skipped} without)")
    return 0


def self_test() -> int:
    ok = parse_frontmatter(
        "---\naudience: [agent]\nsource_files:\n  - foo.rs\nrelated: [a.md]\n---\n# Title\n"
    )
    assert ok == {
        "audience": ["agent"],
        "source_files": ["foo.rs"],
        "related": ["a.md"],
    }
    assert parse_frontmatter("# no fence\n") is None
    try:
        parse_frontmatter("---\naudience: [agent]\n")
    except ValueError:
        pass
    else:
        raise AssertionError("expected unclosed fence to fail")
    errs = validate({"audience": ["nope"]}, Path("x.md"))
    assert errs
    print("frontmatter self-test: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
PY
