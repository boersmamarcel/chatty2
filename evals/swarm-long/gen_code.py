#!/usr/bin/env python3
"""Generate the code-fix family (lc01..lc06) of the long swarm benchmark (EV-7, AGE-826).

Every task is built from a hand-written source directory
`evals/swarm-long/src-code/<id>/`:

    meta.json     {"title": ..., "package": ..., "issues": [
                     {"n": 1, "title": ..., "visible": ["tests.test_x"]}, ...]}
                  `visible` lists the visible test modules that belong to the
                  issue (empty when the issue is specified in prose only).
    workspace/    the broken project exactly as the agent sees it: the
                  package, ISSUES.md, README.md and the visible tests/.
    hidden/       test_issue<n>.py, one module per issue.
    fixes.py      FIXES = {n: [(path, old, new), ...]}: the reference fix of
                  issue n as exact, unique text replacements on workspace files.

Output per task (`evals/swarm-long/tasks/<id>/`):

    task.json     family, title, prompt, deliverable
    workspace/    copied from the source
    hidden/tests/ the visible tests unchanged plus test_issue<n>.py
    solution/     every file the fixes change, fully fixed (a workspace overlay)
    check.json    {"type": "parts", "parts": [one `tests` part per issue]}

There is no randomness: re-running reproduces the bytes exactly. The script
deletes and rewrites only tasks/lc01..lc06.

    python3 gen_code.py            # regenerate
    python3 gen_code.py --check    # regenerate, then run the independence check:
                                   #   blank workspace: every part fails;
                                   #   original + fix n only: part n passes and
                                   #     every other part still fails;
                                   #   all fixes: every part passes and the
                                   #     visible suite (discover) passes.

Python 3.6+, stdlib only.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "src-code")
OUT = os.path.join(HERE, "tasks")
IDS = ["lc01", "lc02", "lc03", "lc04", "lc05", "lc06"]
SKIP = {"__pycache__"}

PROMPT = (
    "This repository is a small Python package with a list of open issues in "
    "ISSUES.md. Resolve every issue listed there: each one is a bug or a small "
    "feature, and its acceptance criteria are stated precisely in ISSUES.md. "
    "Some issues have tests in tests/ already; the others are specified only "
    "in ISSUES.md, and your fix will be checked against that specification by "
    "further tests, so follow it to the letter (names, signatures, error types, "
    "formats, edge cases). Run the tests with: "
    "python3 -m unittest discover -s tests -t . "
    "Do not modify or delete the existing tests (you may add new test files). "
    "Only the Python standard library is available. When you are done, reply "
    "with a short summary of what you changed per issue."
)


def walk(root):
    """Sorted relative paths of every file under root (no caches)."""
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP)
        for name in sorted(filenames):
            if name.endswith(".pyc"):
                continue
            found.append(os.path.relpath(os.path.join(dirpath, name), root))
    return sorted(found)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def write(path, data):
    d = os.path.dirname(path)
    if d and not os.path.isdir(d):
        os.makedirs(d)
    with open(path, "wb") as f:
        f.write(data)


def load_fixes(task_id):
    scope = {}
    path = os.path.join(SRC, task_id, "fixes.py")
    with open(path, encoding="utf-8") as f:
        exec(compile(f.read(), path, "exec"), scope)
    return scope["FIXES"]


def apply_fixes(files, fixes, numbers):
    """files: {rel: bytes}. Returns a new dict with the fixes of `numbers` applied."""
    out = dict(files)
    for n in numbers:
        for rel, old, new in fixes[n]:
            text = out[rel].decode("utf-8")
            count = text.count(old)
            if count != 1:
                raise SystemExit("fix %s: %r occurs %d times in %s" % (n, old[:60], count, rel))
            out[rel] = text.replace(old, new).encode("utf-8")
    return out


def load_task(task_id):
    src = os.path.join(SRC, task_id)
    with open(os.path.join(src, "meta.json"), encoding="utf-8") as f:
        meta = json.load(f)
    ws_root = os.path.join(src, "workspace")
    workspace = {rel: read(os.path.join(ws_root, rel)) for rel in walk(ws_root)}
    hid_root = os.path.join(src, "hidden")
    hidden = {rel: read(os.path.join(hid_root, rel)) for rel in walk(hid_root)}
    return meta, workspace, hidden, load_fixes(task_id)


def parts_of(meta):
    parts = []
    for issue in meta["issues"]:
        n = issue["n"]
        modules = list(issue.get("visible", [])) + ["tests.test_issue%d" % n]
        parts.append({"id": "P%d" % n, "kind": "tests", "desc": "issue %d: %s" % (n, issue["title"]),
                      "modules": modules})
    return parts


def dump_json(path, obj):
    write(path, (json.dumps(obj, indent=2, sort_keys=True) + "\n").encode("utf-8"))


def generate(task_id):
    meta, workspace, hidden, fixes = load_task(task_id)
    out = os.path.join(OUT, task_id)
    if os.path.isdir(out):
        shutil.rmtree(out)
    for rel, data in workspace.items():
        write(os.path.join(out, "workspace", rel), data)
    hidden_tests = {rel: data for rel, data in workspace.items() if rel.startswith("tests" + os.sep)}
    for rel, data in hidden.items():
        hidden_tests[os.path.join("tests", rel)] = data
    for rel, data in hidden_tests.items():
        write(os.path.join(out, "hidden", rel), data)
    solved = apply_fixes(workspace, fixes, sorted(fixes))
    for rel in sorted(solved):
        if solved[rel] != workspace[rel]:
            write(os.path.join(out, "solution", rel), solved[rel])
    dump_json(os.path.join(out, "task.json"), {
        "family": "code-fix",
        "title": meta["title"],
        "prompt": PROMPT,
        "deliverable": "the fixed workspace (every issue in ISSUES.md resolved)",
    })
    dump_json(os.path.join(out, "check.json"), {"type": "parts", "parts": parts_of(meta)})
    return meta, workspace, hidden_tests, fixes


# ---------------------------------------------------------------- self-check

def run_modules(root, modules):
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    try:
        out = subprocess.run([sys.executable, "-m", "unittest"] + modules, cwd=root, env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             universal_newlines=True, timeout=120)
    except subprocess.TimeoutExpired:
        return False
    import re
    ran = re.search(r"Ran (\d+) tests?", out.stdout)
    return out.returncode == 0 and ran is not None and int(ran.group(1)) > 0


def materialize(files, hidden_tests):
    root = tempfile.mkdtemp(prefix="lc-check-")
    for rel, data in files.items():
        if rel.startswith("tests" + os.sep):
            continue
        write(os.path.join(root, rel), data)
    for rel, data in hidden_tests.items():
        write(os.path.join(root, rel), data)
    return root


def score(files, hidden_tests, parts):
    root = materialize(files, hidden_tests)
    try:
        return {p["id"]: run_modules(root, p["modules"]) for p in parts}
    finally:
        shutil.rmtree(root)


def check(task_id, meta, workspace, hidden_tests, fixes):
    parts = parts_of(meta)
    problems = []
    blank = score(workspace, hidden_tests, parts)
    for pid, ok in sorted(blank.items()):
        if ok:
            problems.append("blank workspace passes %s" % pid)
    for issue in meta["issues"]:
        n = issue["n"]
        got = score(apply_fixes(workspace, fixes, [n]), hidden_tests, parts)
        for pid, ok in sorted(got.items()):
            if pid == "P%d" % n and not ok:
                problems.append("fix %d alone does not pass %s" % (n, pid))
            if pid != "P%d" % n and ok and not blank[pid]:
                problems.append("fix %d alone also passes %s" % (n, pid))
    solved = apply_fixes(workspace, fixes, sorted(fixes))
    full = score(solved, hidden_tests, parts)
    for pid, ok in sorted(full.items()):
        if not ok:
            problems.append("reference fails %s" % pid)
    root = tempfile.mkdtemp(prefix="lc-visible-")
    try:
        for rel, data in solved.items():
            write(os.path.join(root, rel), data)
        env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
        out = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."],
                             cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             universal_newlines=True)
        if out.returncode != 0:
            problems.append("visible suite fails on the reference:\n" + out.stdout[-2000:])
        for rel, data in workspace.items():
            write(os.path.join(root, rel), data)
        out = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."],
                             cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             universal_newlines=True)
        if out.returncode == 0:
            problems.append("visible suite already passes on the blank workspace")
    finally:
        shutil.rmtree(root)
    n_blank = sum(1 for ok in blank.values() if ok)
    n_full = sum(1 for ok in full.values() if ok)
    print("%s: %d parts; blank %d/%d, reference %d/%d, independence %s" % (
        task_id, len(parts), n_blank, len(parts), n_full, len(parts),
        "ok" if not problems else "FAILED"))
    for p in problems:
        print("  - " + p)
    return not problems


def main(argv):
    ids = [a for a in argv if not a.startswith("-")] or IDS
    ids = [i for i in ids if os.path.isdir(os.path.join(SRC, i))]
    ok = True
    for task_id in ids:
        meta, workspace, hidden_tests, fixes = generate(task_id)
        if "--check" in argv:
            ok = check(task_id, meta, workspace, hidden_tests, fixes) and ok
        else:
            print("%s: generated" % task_id)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
