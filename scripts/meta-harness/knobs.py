"""knobs.json -> chatty-tui CLI flags (same function as harbor agents/meta_harness.py)."""
import re

_SAFE = re.compile(r"^[A-Za-z0-9_.,:-]+$")


def knob_flags(knobs: dict) -> list:
    out = []
    for key, flag in (("max_agent_turns", "--max-agent-turns"), ("max_duration", "--max-duration"),
                      ("tool_loading", "--tool-loading"), ("tools", "--tools")):
        v = knobs.get(key)
        if v is None:
            continue
        v = str(v)
        if not _SAFE.match(v):
            raise ValueError(f"knob {key}={v!r} has unsafe characters")
        out += [flag, v]
    only = knobs.get("only")
    if only:
        v = ",".join(only)
        if not _SAFE.match(v):
            raise ValueError(f"knob only={v!r} has unsafe characters")
        out += ["--only", v]
    return out
