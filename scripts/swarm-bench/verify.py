#!/usr/bin/env python3
"""The swarm-vs-single benchmark's verifiers (EV-3, AGE-670).

    verify.py <task-dir> <workspace> <answer-file>

Reads the task's `check.json` (which the agent never sees) and prints one
JSON object: {"pass": bool, "reason": str, ...}. Exit code 0 either way;
2 on a broken task.

Three checks, one per family:

- `verdict` (data audit): the last `VERDICT: <x>` line of the final answer
  (else of a report file the run left, see `deliverable_text`) equals the
  task's label, case-insensitively.
- `tests` (code fix): the task's test command passes in the workspace, and
  every protected test file is byte-identical to the task's original.
- `facts` (research then write): the deliverable file (e.g. brief.md) exists,
  matches every fact pattern of the checklist (case-insensitive regex), cites
  at least one corpus file in square brackets, and cites nothing that is not
  a corpus file (no other file name, no URL).

Where a deliverable is looked for: the workspace root, then the workers' git
worktrees under `.chatty/worktrees/`, then any branch of the workspace's
repository. A team's writer works in its own worktree and Chatty commits its
branch; the leader may or may not merge it. Both arms are judged by the same
lookup.

Written for Python 3.6+.
"""

import json
import os
import re
import subprocess
import sys

VERDICT_RE = re.compile(r"VERDICT\s*[:=]\s*(.+)", re.I)
CITE_RE = re.compile(r"\[([^\]\n]+)\]")
FILE_RE = re.compile(r"[\w./-]+\.(?:md|txt|csv|pdf|html?|docx?|json)\b", re.I)
URL_RE = re.compile(r"https?://", re.I)


def git(cwd, *args):
    out = subprocess.run(["git"] + list(args), cwd=cwd, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, universal_newlines=True)
    return out.returncode, out.stdout


def find_file(workspace, name):
    """(text, where) of the newest copy of `name`, or (None, None)."""
    root = os.path.join(workspace, name)
    if os.path.isfile(root):
        return open(root, encoding="utf-8", errors="replace").read(), "workspace"
    worktrees = os.path.join(workspace, ".chatty", "worktrees")
    best = None
    if os.path.isdir(worktrees):
        for wt in sorted(os.listdir(worktrees)):
            p = os.path.join(worktrees, wt, name)
            if os.path.isfile(p):
                mtime = os.path.getmtime(p)
                if best is None or mtime > best[0]:
                    best = (mtime, p, "worktree " + wt)
    if best:
        return open(best[1], encoding="utf-8", errors="replace").read(), best[2]
    code, out = git(workspace, "log", "--all", "-1", "--format=%H", "--", name)
    commit = out.strip()
    if code == 0 and commit:
        code, text = git(workspace, "show", "%s:%s" % (commit, name))
        if code == 0:
            return text, "commit " + commit[:10]
    return None, None


def deliverable_text(workspace, answer):
    """The final answer, then any report a run left: report.md, answer.txt."""
    parts = [answer]
    for name in ("report.md", "answer.txt"):
        text, _ = find_file(workspace, name)
        if text:
            parts.append(text)
    return parts


def norm(x):
    return re.sub(r"[^a-z0-9]+", " ", x.lower()).strip()


def check_verdict(check, workspace, answer):
    choices = {norm(c): c for c in check["choices"]}
    for source, text in zip(("answer", "report"), deliverable_text(workspace, answer)):
        found = VERDICT_RE.findall(text)
        if not found:
            continue
        raw = found[-1].strip()
        value = norm(raw)
        # Accept "<choice>" followed by trailing prose ("APAC (−24,687)").
        match = None
        for key in sorted(choices, key=len, reverse=True):
            if value == key or value.startswith(key + " "):
                match = choices[key]
                break
        ok = match is not None and norm(match) == norm(check["label"])
        return {"pass": ok, "verdict": raw, "parsed": match, "source": source,
                "reason": "verdict matches" if ok else "verdict %r is not %r" % (raw, check["label"])}
    return {"pass": False, "verdict": None, "reason": "no VERDICT line"}


def check_tests(check, task_dir, workspace):
    for rel in check["protected"]:
        original = os.path.join(task_dir, "workspace", rel)
        current = os.path.join(workspace, rel)
        if not os.path.isfile(current) or open(original, "rb").read() != open(current, "rb").read():
            return {"pass": False, "reason": "protected test file changed: %s" % rel}
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    for key in list(env):
        if key.endswith("_API_KEY"):
            env.pop(key)
    try:
        out = subprocess.run(check["command"], cwd=workspace, env=env, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, universal_newlines=True, timeout=120)
    except subprocess.TimeoutExpired:
        return {"pass": False, "reason": "tests timed out"}
    ran = re.search(r"Ran (\d+) tests?", out.stdout)
    ok = out.returncode == 0 and ran is not None and int(ran.group(1)) > 0
    tail = "\n".join(out.stdout.strip().splitlines()[-3:])
    return {"pass": ok, "exit": out.returncode, "tests_ran": int(ran.group(1)) if ran else 0,
            "reason": "tests pass" if ok else "tests fail: " + tail}


def check_facts(check, workspace):
    text, where = find_file(workspace, check["deliverable"])
    if text is None:
        return {"pass": False, "reason": "no %s written" % check["deliverable"]}
    missing = [f["fact"] for f in check["facts"] if not re.search(f["pattern"], text, re.I)]
    corpus = set(check["corpus"])
    cited, foreign = [], []
    for inner in CITE_RE.findall(text):
        for name in FILE_RE.findall(inner):
            base = os.path.basename(name)
            (cited if base in corpus else foreign).append(base)
    if URL_RE.search(text):
        foreign.append("a URL")
    foreign = [f for f in foreign if f != check["deliverable"]]
    reasons = []
    if missing:
        reasons.append("missing facts: " + "; ".join(missing))
    if not cited:
        reasons.append("cites no corpus file")
    if foreign:
        reasons.append("cites non-corpus sources: " + ", ".join(sorted(set(foreign))))
    return {"pass": not reasons, "found_in": where, "missing": missing,
            "cited": sorted(set(cited)), "foreign": sorted(set(foreign)),
            "reason": "; ".join(reasons) or "all facts, corpus-only citations"}


def verify(task_dir, workspace, answer):
    with open(os.path.join(task_dir, "check.json"), encoding="utf-8") as f:
        check = json.load(f)
    kind = check.get("type")
    if kind == "verdict":
        return check_verdict(check, workspace, answer)
    if kind == "tests":
        return check_tests(check, task_dir, workspace)
    if kind == "facts":
        return check_facts(check, workspace)
    sys.stderr.write("verify.py: %s: unknown check type %r\n" % (task_dir, kind))
    sys.exit(2)


def main(argv):
    if len(argv) != 3:
        sys.stderr.write("usage: verify.py <task-dir> <workspace> <answer-file>\n")
        return 2
    task_dir, workspace, answer_file = argv
    answer = ""
    if os.path.isfile(answer_file):
        answer = open(answer_file, encoding="utf-8", errors="replace").read()
    print(json.dumps(verify(task_dir, workspace, answer), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
