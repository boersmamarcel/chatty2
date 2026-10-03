#!/usr/bin/env python3
"""Self-check of the EV-7 long task set (AGE-826): every task's reference
solution must score 100 % and its untouched workspace at most 20 %.

    python3 evals/swarm-long/selfcheck.py [task-id ...]

The reference (`solution/`) is overlaid on a copy of the workspace; for data
and research tasks it holds the deliverable. Python 3.6+, stdlib only.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
VERIFY = os.path.join(ROOT, "scripts", "swarm-bench", "verify.py")
TASKS = os.path.join(HERE, "tasks")


def overlay(src, dst):
    for base, _, files in os.walk(src):
        rel = os.path.relpath(base, src)
        os.makedirs(os.path.join(dst, rel), exist_ok=True)
        for name in files:
            shutil.copy2(os.path.join(base, name), os.path.join(dst, rel, name))


def score(task_dir, with_solution):
    tmp = tempfile.mkdtemp(prefix="selfcheck-")
    try:
        ws = os.path.join(tmp, "ws")
        shutil.copytree(os.path.join(task_dir, "workspace"), ws)
        if with_solution:
            overlay(os.path.join(task_dir, "solution"), ws)
        answer = os.path.join(tmp, "answer.txt")
        open(answer, "w").close()
        out = subprocess.run([sys.executable, VERIFY, task_dir, ws, answer],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
        if out.returncode != 0:
            return {"score": -1, "reason": out.stderr.strip()[-300:]}
        return json.loads(out.stdout)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def main(argv):
    names = argv or sorted(n for n in os.listdir(TASKS) if os.path.isfile(os.path.join(TASKS, n, "task.json")))
    bad = 0
    for name in names:
        task_dir = os.path.join(TASKS, name)
        ref = score(task_dir, True)
        blank = score(task_dir, False)
        ok = ref.get("score") == 1.0 and 0 <= blank.get("score", 1) <= 0.2
        bad += not ok
        print("%-6s %s ref=%-6s blank=%-6s %s" % (name, "ok  " if ok else "FAIL", ref.get("score"),
                                                  blank.get("score"), "" if ok else ref.get("reason", "")))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
