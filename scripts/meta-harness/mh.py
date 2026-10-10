#!/usr/bin/env python3
"""AGE-862 Meta-Harness: experience store, eval queue and CLI.

Layout (MH_ROOT, default /media/marcel/data/rust/swarm-results/age-862):
  tasks.json                      search (40) / test (100) DABstep Hard tasks; never in the store
  store/                          what the proposer sees (its working directory)
    candidates/<id>/harness/      preamble.md, BRIEF.md, helper.py, skills/, knobs.json
    candidates/<id>/meta.json     id, parent, source (baseline|proposer|knob), rationale, created
    candidates/<id>/VALID         written by `validate` when the candidate passed
    candidates/<id>/scores.json   search-set results only (per rep, per task)
    candidates/<id>/traces/search-r<k>/<task>.atif.json   full-run ATIF + usage per task
    ruled-out.md, README.md, .claude/skills/meta-harness-proposer/SKILL.md
  heldout/<id>/                   test-set and EV-7 results (never in the store)
  jobs/<job>/                     harbor job dirs; ev7/<run-id>/ bench output
  queue.txt                       explicit jobs, one per line: "<cand> <set> <rep>" (set: search|test|ev7)
  STOP                            the queue finishes its current job and exits

Commands:
  mh.py queue                     run the eval queue forever (resumable)
  mh.py eval <cand> <set> <rep>   run one job now
  mh.py validate <cand>           check a candidate (exit 1 on failure)
  mh.py frontier | top [k] | diff <a> <b> | show <cand> | status
"""
from __future__ import annotations

import datetime as dt
import glob
import json
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

R = Path(os.environ.get("MH_ROOT", "/media/marcel/data/rust/swarm-results/age-862"))
STORE = R / "store"
CANDS = STORE / "candidates"
HELD = R / "heldout"
JOBS = R / "jobs"
HARBOR = Path("/media/marcel/data/rust/chattyapp/harbor-chatty-dabstep-team")
SHA = (R / "TARBALL_SHA").read_text().strip() if (R / "TARBALL_SHA").exists() else ""
BIN = R / "bin" / f"chatty-tui-{SHA}"
MODEL = "RedHatAI/Qwen3.8-27B-INT4"
VLLM = "http://172.17.0.1:8000/v1"
BASE = "c000-baseline"
SEARCH_REPS = 3            # baseline noise band
TEST_REPS = 2              # prereg: baseline and finalist each k=2 on test and EV-7
EV7_A = "ld01,lr01,lc08,ld04,lr06,lc10,ld07,ld09"
EV7_B = "lc07,ld03,lr03,lc09,ld06,lr08,ld08"
LOG = R / "queue.log"

# One-factor-at-a-time numeric/discrete knob grid (hybrid search, phase A on the
# baseline, phase B on the best proposer candidate). Values chosen before any run.
KNOB_GRID = [
    {"max_agent_turns": 20},
    {"max_agent_turns": 40},
    {"max_duration": "15m"},
    {"max_duration": "20m"},
    {"tool_loading": "dynamic"},
    {"think": True},
    {"only": ["shell", "fs-read", "fs-write", "code-exec"]},
    {"max_agent_turns": 12},
]


def log(msg: str) -> None:
    line = f"{dt.datetime.now().isoformat(timespec='seconds')} {msg}"
    print(line, flush=True)
    with open(LOG, "a") as f:
        f.write(line + "\n")


def tasks(set_name: str) -> list[dict]:
    return json.loads((R / "tasks.json").read_text())[set_name]


def tnum(name: str) -> str:
    return name.split("/")[-1]


# ── scores ──────────────────────────────────────────────────────────────────

def scores_path(cand: str, set_name: str) -> Path:
    return CANDS / cand / "scores.json" if set_name == "search" else HELD / cand / "scores.json"


def load_scores(cand: str, set_name: str) -> dict:
    p = scores_path(cand, set_name)
    return json.loads(p.read_text()) if p.exists() else {"runs": {}}


def summarize(sc: dict, prefix: str) -> dict:
    runs = {k: v for k, v in sc["runs"].items() if k.startswith(prefix + "-r") and v.get("complete")}
    if not runs:
        return {}
    means = [v["mean"] for v in runs.values()]
    tok = [v["tokens"] for v in runs.values()]
    solved = sum(v["solved"] for v in runs.values())
    return {"reps": len(runs), "mean": statistics.mean(means), "rep_means": means,
            "sd": statistics.stdev(means) if len(means) > 1 else None,
            "tokens_per_task": statistics.mean(t / max(1, r["n"]) for t, r in zip(tok, runs.values())),
            "tokens_per_solved": sum(tok) / max(1e-9, solved)}


def save_run(cand: str, set_name: str, rep: int, per_task: dict, complete: bool) -> None:
    sc = load_scores(cand, set_name)
    vals = [v["reward"] for v in per_task.values()]
    sc["runs"][f"{set_name}-r{rep}"] = {
        "complete": complete, "n": len(vals), "mean": statistics.mean(vals) if vals else 0.0,
        "solved": sum(vals), "tokens": sum(v.get("tokens", 0) for v in per_task.values()),
        "tasks": per_task, "binary": SHA, "at": dt.datetime.now().isoformat(timespec="seconds")}
    sc["summary"] = {s: summarize(sc, s) for s in ("search", "test", "ev7") if summarize(sc, s)}
    p = scores_path(cand, set_name)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(sc, indent=1, sort_keys=True))
    tmp.replace(p)


def usage_tokens(path: Path) -> int:
    try:
        u = json.loads(path.read_text())
        return int(u.get("input_tokens") or 0) + int(u.get("output_tokens") or 0)
    except Exception:  # noqa: BLE001
        return 0


# ── DABstep via harbor ─────────────────────────────────────────────────────

def knobs_of(cand: str) -> dict:
    p = CANDS / cand / "harness" / "knobs.json"
    return json.loads(p.read_text()) if p.exists() else {}


def job_config(cand: str, set_name: str, rep: int) -> tuple[str, Path]:
    name = f"{cand}-{set_name}-r{rep}"
    knobs = knobs_of(cand)
    cfg = {
        "job_name": name, "jobs_dir": str(JOBS / name), "n_attempts": 1,
        "n_concurrent_trials": 2, "quiet": True,
        "agents": [{"name": "agents.meta_harness:MetaHarnessArm", "kwargs": {
            "provider_url": VLLM, "api_key_env": "OLLAMA_API_KEY", "model": MODEL,
            "think": bool(knobs.get("think", False)), "tarball_sha": SHA,
            "candidate_dir": str(CANDS / cand / "harness"), "pip_packages": "duckdb pandas"}}],
        "tasks": tasks(set_name),
    }
    p = JOBS / f"{name}.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(cfg, indent=1))
    return name, p


def collect_harbor(cand: str, set_name: str, rep: int, complete: bool) -> dict:
    name = f"{cand}-{set_name}-r{rep}"
    trace_dir = (CANDS / cand / "traces" if set_name == "search" else HELD / cand / "traces") / f"{set_name}-r{rep}"
    trace_dir.mkdir(parents=True, exist_ok=True)
    per: dict = {}
    for rj in glob.glob(str(JOBS / name / name / "*" / "result.json")):
        d = json.loads(Path(rj).read_text())
        tname = d.get("task_name")
        if not tname:
            continue
        vr = d.get("verifier_result") or {}
        rw = (vr.get("rewards") or {}).get("reward")
        agent = Path(rj).parent / "agent"
        exc = d.get("exception_info")
        per[tname] = {"reward": 0.0 if rw is None else float(rw),
                      "tokens": usage_tokens(agent / "usage.json"),
                      "error": (exc or {}).get("exception_type") if exc else None}
        for src, dst in (("atif.json", f"{tnum(tname)}.atif.json"), ("usage.json", f"{tnum(tname)}.usage.json"),
                         ("stdout.txt", f"{tnum(tname)}.trace.txt")):
            if (agent / src).exists():
                shutil.copyfile(agent / src, trace_dir / dst)
    save_run(cand, set_name, rep, per, complete)
    return per


def run_harbor(cand: str, set_name: str, rep: int) -> int:
    name, cfg = job_config(cand, set_name, rep)
    done = JOBS / name / "DONE"
    if done.exists():
        return 0
    env = dict(os.environ, OLLAMA_API_KEY="unused", PYTHONPATH=".")
    if (JOBS / name / name).is_dir():
        cmd = ["uv", "run", "harbor", "job", "resume", "-p", str(JOBS / name / name)]
    else:
        cmd = ["uv", "run", "harbor", "run", "-c", str(cfg), "--agent-timeout-multiplier", "1.3334", "-y"]
    log(f"start {name}")
    with open(JOBS / f"{name}.log", "a") as lf:
        rc = subprocess.call(cmd, cwd=HARBOR, env=env, stdout=lf, stderr=subprocess.STDOUT)
    per = collect_harbor(cand, set_name, rep, complete=(rc == 0))
    n_expected = len(tasks(set_name))
    log(f"end {name} rc={rc} n={len(per)}/{n_expected} mean={statistics.mean([v['reward'] for v in per.values()]) if per else 0:.3f}")
    if rc == 0 and len(per) >= n_expected:
        done.touch()
    return rc


# ── EV-7 via the swarm-bench (host chatty-tui through a candidate wrapper) ──

def ev7_wrapper(cand: str) -> Path:
    """A chatty-tui stand-in that adds the candidate's preamble, knobs and skills."""
    h = CANDS / cand / "harness"
    w = HELD / cand / "chatty-tui-wrapper"
    w.parent.mkdir(parents=True, exist_ok=True)
    w.write_text(f"""#!{sys.executable}
import json, os, shutil, sys
sys.path.insert(0, {str(Path(__file__).resolve().parent)!r})
from knobs import knob_flags
H = {str(h)!r}
args = sys.argv[1:]
knobs = json.load(open(os.path.join(H, "knobs.json"))) if os.path.exists(os.path.join(H, "knobs.json")) else {{}}
extra = knob_flags(knobs)
overridden = {{extra[i] for i in range(0, len(extra), 2)}}
out, i = [], 0
while i < len(args):
    if args[i] in overridden:
        i += 2
        continue
    out.append(args[i]); i += 1
ws = out[out.index("--workspace") + 1] if "--workspace" in out else None
if ws and os.path.isdir(os.path.join(H, "skills")):
    shutil.copytree(os.path.join(H, "skills"), os.path.join(ws, ".claude", "skills"), dirs_exist_ok=True)
if "--usage-file" in out and "--participant-fd" not in out:
    d = os.path.dirname(out[out.index("--usage-file") + 1])
    out += ["--export-atif", os.path.join(d, "atif.json")]
if "--participant-fd" not in out:
    out += ["--preamble", open(os.path.join(H, "preamble.md")).read()]
os.execv({str(BIN)!r}, [{str(BIN)!r}] + out + extra)
""")
    w.chmod(0o755)
    return w


def run_ev7(cand: str, rep: int) -> int:
    run_id = f"{cand}-ev7-r{rep}"
    out = JOBS / "ev7"
    done = out / f"{run_id}.DONE"
    if done.exists():
        return 0
    wrapper = ev7_wrapper(cand)
    bench = R / "ev7bench" / "scripts" / "swarm-bench" / "bench.py"
    think = "true" if knobs_of(cand).get("think") else "false"
    procs = []
    log(f"start {run_id}")
    for s, only in (("a", EV7_A), ("b", EV7_B)):
        cmd = [sys.executable, str(bench), "--provider", "openai-compat", "--model", MODEL,
               "--think", think, "--arm", "single", "--tasks", str(R / "ev7bench/evals/swarm-long/tasks"),
               "--only", only, "--rep", str(rep), "--max-turns", "100", "--max-duration", "30m",
               "--run-timeout", "3000", "--hold-max-s", "150", "--throttle-max", "2",
               "--prereg", str(R / "wt/docs/research/meta-harness-prereg.md"), "--chatty-tui", str(wrapper),
               "--out", str(out), "--run-id", f"{run_id}-{s}"]
        out.mkdir(parents=True, exist_ok=True)
        lf = open(JOBS / f"{run_id}-{s}.log", "a")
        procs.append(subprocess.Popen(cmd, cwd=R / "ev7bench", stdout=lf, stderr=subprocess.STDOUT))
        time.sleep(20)
    rcs = [p.wait() for p in procs]
    per = {}
    for rj in glob.glob(str(out / f"{run_id}-?" / "runs" / "*" / "single" / "result.json")):
        d = json.loads(Path(rj).read_text())
        task = Path(rj).parent.parent.name
        m = d.get("meter") or {}
        per[task] = {"reward": float((d.get("check") or {}).get("score", 0.0)),
                     "tokens": int(m.get("input_tokens", 0)) + int(m.get("output_tokens", 0)),
                     "error": None if d.get("complete") else "incomplete"}
    save_run(cand, "ev7", rep, per, complete=all(r == 0 for r in rcs))
    log(f"end {run_id} rcs={rcs} n={len(per)}/15 mean={statistics.mean([v['reward'] for v in per.values()]) if per else 0:.3f}")
    if all(r == 0 for r in rcs) and len(per) >= 15:
        done.touch()
    return max(rcs)


def run_job(cand: str, set_name: str, rep: int) -> int:
    if not (CANDS / cand / "VALID").exists():
        log(f"skip {cand}: not validated")
        return 2
    return run_ev7(cand, rep) if set_name == "ev7" else run_harbor(cand, set_name, rep)


def job_done(cand: str, set_name: str, rep: int) -> bool:
    if set_name == "ev7":
        return (JOBS / "ev7" / f"{cand}-ev7-r{rep}.DONE").exists()
    return (JOBS / f"{cand}-{set_name}-r{rep}" / "DONE").exists()


# ── validation ─────────────────────────────────────────────────────────────

ALLOWED_FILES = {"preamble.md", "BRIEF.md", "helper.py", "knobs.json"}
KNOB_KEYS = {"max_agent_turns", "max_duration", "tool_loading", "tools", "only", "think"}
FORBIDDEN_RE = re.compile(r"adyen/\d+|expected_answer|\bgold\b|ground[_ ]truth|/tests/|verifier|grader|answers?\.json|reward",
                          re.I)


def validate(cand: str) -> list[str]:
    h = CANDS / cand / "harness"
    errs: list[str] = []
    if not (h / "preamble.md").is_file():
        errs.append("missing preamble.md")
    for f in h.rglob("*"):
        if f.is_dir():
            continue
        rel = f.relative_to(h).as_posix()
        if not (rel in ALLOWED_FILES or re.fullmatch(r"skills/[a-z0-9-]+/SKILL\.md", rel)):
            errs.append(f"file not allowed: {rel}")
        if f.stat().st_size > 64_000:
            errs.append(f"too large: {rel}")
    if (h / "knobs.json").exists():
        try:
            k = json.loads((h / "knobs.json").read_text())
            bad = set(k) - KNOB_KEYS
            if bad:
                errs.append(f"unknown knobs {sorted(bad)}")
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            from knobs import knob_flags  # noqa: PLC0415
            knob_flags(k)
            if k.get("tool_loading") not in (None, "all", "dynamic"):
                errs.append("tool_loading must be all|dynamic")
            if k.get("tools") not in (None, "coordinator", "coder", "reviewer"):
                errs.append("tools must be coordinator|coder|reviewer")
            groups = {"shell", "fs-read", "fs-write", "fetch", "git", "code-exec", "docker-exec"}
            if k.get("only") and not set(k["only"]) <= groups:
                errs.append("only: unknown group")
            if k.get("max_agent_turns") is not None and not (0 <= int(k["max_agent_turns"]) <= 200):
                errs.append("max_agent_turns out of range")
        except Exception as exc:  # noqa: BLE001
            errs.append(f"knobs.json: {exc}")
    # Leakage: task numbers of either split, grader words, answers seen in no file but the trace.
    ids = {tnum(t["name"]) for s in ("search", "test") for t in tasks(s)}
    base = CANDS / BASE / "harness"
    for f in h.rglob("*"):
        if f.is_dir() or f.name == "knobs.json":
            continue
        text = f.read_text(errors="replace")
        rel = f.relative_to(h).as_posix()
        old = (base / rel).read_text(errors="replace") if (base / rel).exists() else ""
        new_lines = [ln for ln in text.splitlines() if ln not in set(old.splitlines())]
        for ln in new_lines:
            if FORBIDDEN_RE.search(ln):
                errs.append(f"{rel}: forbidden term in added line: {ln.strip()[:100]}")
            for tok in re.findall(r"(?<![\w.])\d{2,4}(?![\w.])", ln):
                if tok in ids and re.search(r"task|question|id|#", ln, re.I):
                    errs.append(f"{rel}: task id {tok} in added line: {ln.strip()[:100]}")
    if (h / "helper.py").exists():
        try:
            compile((h / "helper.py").read_text(), "helper.py", "exec")
        except SyntaxError as exc:
            errs.append(f"helper.py does not compile: {exc}")
    meta = CANDS / cand / "meta.json"
    if not meta.exists():
        errs.append("missing meta.json")
    else:
        m = json.loads(meta.read_text())
        if not m.get("rationale") or not m.get("changes"):
            errs.append("meta.json needs rationale and changes (one line each, tied to traces)")
    return errs


# ── candidates: knobs ──────────────────────────────────────────────────────

def make_knob_candidate(parent: str, knob: dict) -> str | None:
    tag = "-".join(f"{k}{'_'.join(v) if isinstance(v, list) else v}" for k, v in knob.items())
    tag = re.sub(r"[^a-z0-9_-]", "", tag.lower())[:40]
    cid = f"k-{parent.split('-')[0]}-{tag}"
    if (CANDS / cid).exists():
        return None
    shutil.copytree(CANDS / parent / "harness", CANDS / cid / "harness")
    subprocess.call(["chmod", "-R", "u+w", str(CANDS / cid)])
    k = knobs_of(parent)
    k.update(knob)
    (CANDS / cid / "harness" / "knobs.json").write_text(json.dumps(k, indent=1))
    (CANDS / cid / "meta.json").write_text(json.dumps({
        "id": cid, "parent": parent, "source": "knob", "created": dt.datetime.now().isoformat(timespec="seconds"),
        "rationale": f"hybrid knob search: one-factor variant {knob} of {parent}",
        "changes": [f"knobs.json: {knob}"]}, indent=1))
    errs = validate(cid)
    if errs:
        log(f"knob candidate {cid} invalid: {errs}")
        return None
    (CANDS / cid / "VALID").touch()
    log(f"knob candidate {cid} created")
    return cid


def search_mean(cand: str) -> float | None:
    s = load_scores(cand, "search").get("summary", {}).get("search")
    return s["mean"] if s else None


def next_knob_job() -> tuple[str, str, int] | None:
    # Phase A: the grid on the baseline; phase B: the grid on the best proposer candidate.
    parents = [BASE]
    props = [c.name for c in CANDS.iterdir() if c.name.startswith("p") and search_mean(c.name) is not None]
    if props:
        parents.insert(0, max(props, key=lambda c: search_mean(c)))
    for parent in parents:
        for knob in KNOB_GRID:
            cid = make_knob_candidate(parent, knob)
            if cid:
                return cid, "search", 1
    return None


# ── queue ──────────────────────────────────────────────────────────────────

def explicit_jobs() -> list[tuple[str, str, int]]:
    q = R / "queue.txt"
    out = []
    if q.exists():
        for ln in q.read_text().splitlines():
            p = ln.split("#")[0].split()
            if len(p) == 3:
                out.append((p[0], p[1], int(p[2])))
    return out


def next_job() -> tuple[str, str, int] | None:
    for rep in range(1, SEARCH_REPS + 1):
        if not job_done(BASE, "search", rep):
            return BASE, "search", rep
    for j in explicit_jobs():
        if not job_done(*j):
            return j
    pending = sorted((c for c in CANDS.iterdir() if c.name.startswith("p") and (c / "VALID").exists()
                      and not job_done(c.name, "search", 1)), key=lambda c: c.stat().st_mtime)
    if pending:
        return pending[0].name, "search", 1
    for set_name in ("test", "ev7"):
        for rep in range(1, TEST_REPS + 1):
            if not job_done(BASE, set_name, rep):
                return BASE, set_name, rep
    # knob candidates whose search-r1 is not finished (resume) before making new ones
    for c in sorted(CANDS.iterdir()):
        if c.name.startswith("k-") and (c / "VALID").exists() and not job_done(c.name, "search", 1):
            return c.name, "search", 1
    return next_knob_job()


def queue() -> None:
    failures: dict = {}
    log("queue started")
    while not (R / "STOP").exists():
        j = next_job()
        if j is None:
            time.sleep(120)
            continue
        rc = run_job(*j)
        if rc != 0:
            failures[j] = failures.get(j, 0) + 1
            if failures[j] >= 2:
                log(f"job {j} failed twice; parking it (add to queue.txt again to retry)")
                (JOBS / f"{j[0]}-{j[1]}-r{j[2]}").mkdir(parents=True, exist_ok=True)
                (JOBS / f"{j[0]}-{j[1]}-r{j[2]}" / "PARKED").touch()
                if j[1] != "ev7":
                    (JOBS / f"{j[0]}-{j[1]}-r{j[2]}" / "DONE").touch()
                else:
                    (JOBS / "ev7" / f"{j[0]}-ev7-r{j[2]}.DONE").touch()
            time.sleep(30)
    log("queue stopped (STOP file)")


# ── CLI views ──────────────────────────────────────────────────────────────

def rows() -> list[dict]:
    out = []
    for c in sorted(CANDS.iterdir()):
        s = load_scores(c.name, "search").get("summary", {}).get("search")
        if s:
            out.append({"id": c.name, **s})
    return out


def baseline_band() -> tuple[float, float] | None:
    s = load_scores(BASE, "search").get("summary", {}).get("search")
    if not s or s["reps"] < 2:
        return None
    return min(s["rep_means"]), max(s["rep_means"])


def fmt(r: dict) -> str:
    sd = f"{r['sd']:.3f}" if r.get("sd") is not None else "  -  "
    return f"{r['id']:<44} acc={r['mean']:.3f} reps={r['reps']} sd={sd} tok/task={r['tokens_per_task']:>9.0f}"


def frontier() -> list[dict]:
    rs = rows()
    front = [r for r in rs if not any(
        o["mean"] >= r["mean"] and o["tokens_per_task"] <= r["tokens_per_task"]
        and (o["mean"] > r["mean"] or o["tokens_per_task"] < r["tokens_per_task"]) for o in rs)]
    return sorted(front, key=lambda r: -r["mean"])


def diff(a: str, b: str) -> None:
    subprocess.call(["diff", "-ru", str(CANDS / a / "harness"), str(CANDS / b / "harness")])
    ta = {k: v for run in load_scores(a, "search")["runs"].values() for k, v in run["tasks"].items()}
    tb = {k: v for run in load_scores(b, "search")["runs"].values() for k, v in run["tasks"].items()}

    def per(cand: str) -> dict:
        acc: dict = {}
        for run in load_scores(cand, "search")["runs"].values():
            for k, v in run["tasks"].items():
                acc.setdefault(k, []).append(v["reward"])
        return {k: statistics.mean(v) for k, v in acc.items()}
    pa, pb = per(a), per(b)
    print(f"\nper-task search reward (mean over reps): {a} -> {b}")
    for k in sorted(set(pa) | set(pb), key=lambda x: int(tnum(x))):
        if pa.get(k) != pb.get(k):
            print(f"  {k:<12} {pa.get(k, float('nan')):.2f} -> {pb.get(k, float('nan')):.2f}")
    del ta, tb


def status() -> None:
    band = baseline_band()
    print("baseline search band:", band)
    for r in sorted(rows(), key=lambda r: -r["mean"]):
        print(fmt(r))
    print("queue: tail", LOG)


def main(argv: list[str]) -> int:
    cmd = argv[0] if argv else "status"
    if cmd == "queue":
        queue()
    elif cmd == "eval":
        return run_job(argv[1], argv[2], int(argv[3]))
    elif cmd == "validate":
        errs = validate(argv[1])
        if errs:
            print("INVALID:\n  " + "\n  ".join(errs))
            return 1
        (CANDS / argv[1] / "VALID").touch()
        print("valid")
    elif cmd == "frontier":
        print("baseline search band (min,max of rep means):", baseline_band())
        for r in frontier():
            print(fmt(r))
    elif cmd == "top":
        k = int(argv[1]) if len(argv) > 1 else 5
        for r in sorted(rows(), key=lambda r: -r["mean"])[:k]:
            print(fmt(r))
    elif cmd == "diff":
        diff(argv[1], argv[2])
    elif cmd == "show":
        c = CANDS / argv[1]
        print((c / "meta.json").read_text() if (c / "meta.json").exists() else "{}")
        print(json.dumps(load_scores(argv[1], "search").get("summary", {}), indent=1))
    elif cmd == "status":
        status()
    elif cmd == "collect":
        collect_harbor(argv[1], argv[2], int(argv[3]), complete=False)
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    random.seed(0)
    sys.exit(main(sys.argv[1:]))
