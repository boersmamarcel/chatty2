"""AGE-862 Meta-Harness adapter: one candidate harness directory on a pinned binary.

A candidate directory (the experience store's ``candidates/<id>/harness/``) holds:

  preamble.md        required; passed as --preamble (system-prompt policy)
  BRIEF.md           optional; uploaded to /app/BRIEF.md (workspace conventions)
  helper.py          optional; uploaded to /app/helper.py
  skills/<n>/SKILL.md optional; uploaded to /app/.claude/skills/ (chatty's skill search path)
  knobs.json         optional; CLI knobs: max_agent_turns, max_duration, tool_loading, tools,
                     only (list of tool groups). ``think`` is set in the job config, not here.

Every run records usage.json, conversation.json and a full-run ATIF export
(``--export-atif``, chatty v0.7.0), copied into the trial's agent log dir.
The candidate files live outside the workspace (/opt/mh) except the ones a
workspace is meant to show the model (BRIEF.md, helper.py, skills).
"""

from __future__ import annotations

import json
import re
import shlex
from pathlib import Path
from typing import TYPE_CHECKING

from pydantic import Field

from agents.chatty_crosscheck import SingleArm, SingleArmOptions, _REAL_BIN
from agents.chatty_headless import _CONTAINER_BIN, ChattyHeadlessAgent

if TYPE_CHECKING:
    from harbor.environments.base import BaseEnvironment
    from harbor.models.agent.context import AgentContext

_USAGE = "/tmp/mh-usage.json"
_CONV = "/tmp/mh-conversation.json"
_ATIF = "/tmp/mh-atif.json"
_SAFE = re.compile(r"^[A-Za-z0-9_.,:-]+$")


def knob_flags(knobs: dict) -> list[str]:
    """CLI flags for a knobs.json; values are validated to need no quoting."""
    out: list[str] = []
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


class MetaHarnessOptions(SingleArmOptions):
    candidate_dir: str = Field(description="candidate harness directory (see module doc)")


class MetaHarnessArm(SingleArm):
    options_model = MetaHarnessOptions

    @staticmethod
    def name() -> str:
        return "chatty-meta-harness-age862"

    async def install(self, environment: "BaseEnvironment") -> None:
        await ChattyHeadlessAgent.install(self, environment)
        cdir = Path(self.options.candidate_dir)  # type: ignore[attr-defined]
        knobs = json.loads((cdir / "knobs.json").read_text()) if (cdir / "knobs.json").is_file() else {}
        flags = " ".join(knob_flags(knobs))
        await self.exec_as_root(environment, command="mkdir -p /opt/mh && chmod 755 /opt/mh")
        await environment.upload_file(source_path=cdir / "preamble.md", target_path="/opt/mh/preamble.md")
        wrapper = (
            "#!/bin/sh\n"
            'case " $* " in *" --participant-fd "*) exec ' + _REAL_BIN + ' "$@";; esac\n'
            f'exec {_REAL_BIN} "$@" --usage-file {_USAGE} --save-conversation {_CONV} '
            f'--export-atif {_ATIF} --preamble "$(cat /opt/mh/preamble.md)" {flags}\n'
        )
        await self.exec_as_root(
            environment,
            command=(
                f"chmod 644 /opt/mh/preamble.md && mv {_CONTAINER_BIN} {_REAL_BIN} && "
                f"printf %s {shlex.quote(wrapper)} > {_CONTAINER_BIN} && chmod 755 {_CONTAINER_BIN}"
            ),
        )
        for name in ("BRIEF.md", "helper.py"):
            if (cdir / name).is_file():
                await environment.upload_file(source_path=cdir / name, target_path=f"/app/{name}")
        skills = cdir / "skills"
        if skills.is_dir():
            for f in sorted(skills.rglob("*")):
                if f.is_file():
                    rel = f.relative_to(skills).as_posix()
                    target = f"/app/.claude/skills/{rel}"
                    await self.exec_as_root(environment, command=f"mkdir -p {shlex.quote(str(Path(target).parent))}")
                    await environment.upload_file(source_path=f, target_path=target)
        if getattr(self.options, "pip_packages", None):
            p = self.options.pip_packages  # type: ignore[attr-defined]
            await self.exec_as_root(
                environment,
                command=f"pip3 install --break-system-packages --quiet {p} 2>&1 || pip3 install --quiet {p} 2>&1 || true",
            )
        await self.exec_as_root(environment, command="chmod -R a+rX /app/.claude 2>/dev/null; chmod a+r /app/BRIEF.md /app/helper.py 2>/dev/null; true")

    async def run(self, instruction: str, environment: "BaseEnvironment", context: "AgentContext") -> None:
        try:
            await ChattyHeadlessAgent.run(self, instruction, environment, context)
        finally:
            self.logs_dir.mkdir(parents=True, exist_ok=True)
            for remote, local in ((_USAGE, "usage.json"), (_CONV, "conversation.json"), (_ATIF, "atif.json")):
                try:
                    r = await environment.exec(command=f"cat {remote} 2>/dev/null || true")
                    (self.logs_dir / local).write_text(r.stdout or "")
                except Exception as exc:  # noqa: BLE001 - best effort, recorded
                    (self.logs_dir / local).write_text(f"[copy failed: {exc}]")
