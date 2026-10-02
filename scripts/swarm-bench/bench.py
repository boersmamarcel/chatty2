#!/usr/bin/env python3
"""The swarm-vs-single benchmark's runner (EV-3, AGE-670). Start it through
run.sh; the protocol is docs/research/swarm-vs-single-prereg.md.

Per task and arm: a fresh copy of the task's workspace becomes a git
repository, then one `chatty-tui --headless` run does the task:

- arm `single`: one harness agent, no tool profile, no team: every tool the
  execution settings allow, which covers the union of the team's profiles;
- arm `swarm`: the family's frozen team preset (`--team <preset>`), its
  leader and workers exactly as compiled into this build.

Both arms get the same prompt, the same execution settings and the same
model. The task's verifier (verify.py) then judges the workspace and the
final answer.

Every model call goes through a local pass-through meter, which records each
call's token usage (the cost metric, counted at the wire, workers included)
and, against a vLLM, holds a request back while the server already runs
`--throttle-max` requests (the server is shared).

Results go to <out>/<run-id>/: meta.json and runs/<task>/<arm>/ with
result.json, answer.txt, stderr.log, usage.json and, for code tasks, the
workspace's diff. A finished run is skipped on a re-run with the same
run id, so an interrupted batch resumes. report.py turns a results directory
into the report.

Written for Python 3.6+: this workstation's pyenv default is 3.6.
"""

import argparse
import datetime
import hashlib
import http.client
import http.server
import json
import os
import re
import shutil
import signal
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from urllib.parse import urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PREREG = os.path.join(ROOT, "docs", "research", "swarm-vs-single-prereg.md")
DEFAULT_TASKS = os.path.join(HERE, "tasks")
DEFAULT_OUT = os.path.join(ROOT, "target", "swarm-bench")
VLLM_URL = "http://172.17.0.1:8000/v1"

# The frozen team preset each family's swarm arm runs (compiled into chatty).
FAMILY_PRESET = {
    "data-audit": "data-analysis",
    "code-fix": "fix-and-verify",
    "research-write": "research-brief",
}
ARMS = ("single", "swarm")
GIT_ID = ["-c", "user.name=swarm-bench", "-c", "user.email=swarm-bench@chatty.invalid",
          "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]


def die(message):
    sys.stderr.write("swarm-bench: %s\n" % message)
    sys.exit(2)


def log(message):
    sys.stderr.write("[%s] %s\n" % (time.strftime("%H:%M:%S"), message))
    sys.stderr.flush()


def now_iso():
    return datetime.datetime.utcnow().replace(microsecond=0).isoformat() + "Z"


def sha256_file(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def sha256_tree(directory):
    """One hash over every file's relative path and bytes, in sorted order."""
    h = hashlib.sha256()
    for base, dirs, files in os.walk(directory):
        dirs.sort()
        dirs[:] = [d for d in dirs if d != "__pycache__"]
        for name in sorted(files):
            path = os.path.join(base, name)
            h.update(os.path.relpath(path, directory).encode() + b"\0")
            h.update(open(path, "rb").read() + b"\0")
    return h.hexdigest()


# ── Tasks ───────────────────────────────────────────────────────────────────

def load_tasks(directory, only):
    tasks = []
    for name in sorted(os.listdir(directory)):
        path = os.path.join(directory, name)
        spec = os.path.join(path, "task.json")
        if not os.path.isfile(spec):
            continue
        with open(spec, encoding="utf-8") as f:
            data = json.load(f)
        for key in ("family", "title", "prompt"):
            if not data.get(key):
                die("%s: missing '%s'" % (spec, key))
        if data["family"] not in FAMILY_PRESET:
            die("%s: unknown family %r" % (spec, data["family"]))
        for part in ("workspace", "check.json"):
            if not os.path.exists(os.path.join(path, part)):
                die("%s: missing %s" % (path, part))
        data["name"] = name
        data["dir"] = path
        tasks.append(data)
    if only:
        wanted = set(only)
        unknown = wanted - set(t["name"] for t in tasks)
        if unknown:
            die("--only names unknown tasks: %s" % ", ".join(sorted(unknown)))
        tasks = [t for t in tasks if t["name"] in wanted]
    if not tasks:
        die("no tasks under %s" % directory)
    return interleave(tasks)


def interleave(tasks):
    """The pre-registered order: one task of each family in turn (d01, c01,
    r01, d02, …), so any prefix of the run is a balanced subset."""
    by_family = {}
    for t in tasks:
        by_family.setdefault(t["family"], []).append(t)
    order = [f for f in ("data-audit", "code-fix", "research-write") if f in by_family]
    out = []
    i = 0
    while any(i < len(by_family[f]) for f in order):
        for f in order:
            if i < len(by_family[f]):
                out.append(by_family[f][i])
        i += 1
    return out


# ── git ─────────────────────────────────────────────────────────────────────

def git(cwd, *args):
    out = subprocess.run(["git"] + GIT_ID + list(args), cwd=cwd, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, universal_newlines=True)
    if out.returncode != 0:
        raise RuntimeError("git %s failed in %s: %s" % (" ".join(args), cwd, out.stderr.strip()))
    return out.stdout


def prepare_workspace(task, path):
    shutil.copytree(os.path.join(task["dir"], "workspace"), path)
    git(path, "init", "-q")
    git(path, "add", "-A")
    git(path, "commit", "-q", "-m", "task: %s" % task["name"])
    return git(path, "rev-parse", "HEAD").strip()


# ── The meter: token usage at the wire, and the shared-server throttle ──────

class _Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def vllm_load(metrics_url):
    """Requests the server is running plus those waiting, or None."""
    try:
        text = urllib.request.urlopen(metrics_url, timeout=5).read().decode("utf-8", "replace")
    except Exception:
        return None
    total = 0.0
    seen = False
    for line in text.splitlines():
        m = re.match(r"vllm:num_requests_(running|waiting)\{[^}]*\}\s+([0-9.eE+-]+)", line)
        if m:
            total += float(m.group(2))
            seen = True
    return total if seen else None


class Meter(object):
    """A pass-through proxy in front of an OpenAI-compatible server.

    It forwards every line as it arrives (a buffering proxy stalls a
    streaming client, AGE-460) and records each response's `usage` object.
    With `metrics_url` set it first waits, up to `hold_max_s`, while the
    server's running + waiting requests are at `throttle_max` or more."""

    def __init__(self, upstream, metrics_url=None, throttle_max=2, hold_max_s=90.0):
        parts = urlsplit(upstream)
        self.host = parts.hostname
        self.port = parts.port or (443 if parts.scheme == "https" else 80)
        self.https = parts.scheme == "https"
        self.prefix = parts.path.rstrip("/")
        self.metrics_url = metrics_url
        self.throttle_max = throttle_max
        self.hold_max_s = hold_max_s
        self.records = []
        self.lock = threading.Lock()
        meter = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                meter.forward(self)

            def do_POST(self):
                meter.forward(self)

        self.server = _Server(("127.0.0.1", 0), Handler)
        self.url = "http://127.0.0.1:%d/v1" % self.server.server_address[1]
        thread = threading.Thread(target=self.server.serve_forever)
        thread.daemon = True
        thread.start()

    def hold(self):
        if not self.metrics_url:
            return 0.0
        start = time.time()
        while time.time() - start < self.hold_max_s:
            load = vllm_load(self.metrics_url)
            if load is None or load < self.throttle_max:
                break
            time.sleep(1.0)
        return time.time() - start

    def forward(self, handler):
        length = int(handler.headers.get("Content-Length") or 0)
        body = handler.rfile.read(length) if length else None
        is_chat = handler.path.endswith("/chat/completions")
        held = self.hold() if is_chat else 0.0
        path = handler.path
        if path.startswith("/v1"):
            path = self.prefix + path[3:]
        cls = http.client.HTTPSConnection if self.https else http.client.HTTPConnection
        upstream = cls(self.host, self.port, timeout=3600)
        headers = {}
        for key in ("Content-Type", "Authorization", "Accept"):
            if handler.headers.get(key):
                headers[key] = handler.headers[key]
        if body is not None:
            headers["Content-Length"] = str(len(body))
        started = time.time()
        try:
            upstream.request(handler.command, path, body=body, headers=headers)
            response = upstream.getresponse()
        except OSError as error:
            handler.send_error(502, "swarm-bench meter: %s" % error)
            return
        handler.send_response(response.status)
        content_type = re.sub(r"[\r\n]", "", response.getheader("Content-Type") or "")
        if content_type:
            handler.send_header("Content-Type", content_type)
        handler.send_header("Connection", "close")
        handler.end_headers()
        handler.close_connection = True
        usage = None
        whole = []
        while True:
            line = response.readline()
            if not line:
                break
            try:
                handler.wfile.write(line)
                handler.wfile.flush()
            except OSError:
                break
            if is_chat:
                usage = self.usage_of(line) or usage
                if not line.startswith(b"data:"):
                    whole.append(line)
        if is_chat and usage is None and whole:
            usage = self.usage_of(b"".join(whole))
        upstream.close()
        if is_chat:
            with self.lock:
                self.records.append({
                    "start": started, "end": time.time(), "held_s": held,
                    "status": response.status,
                    "input": (usage or {}).get("prompt_tokens"),
                    "output": (usage or {}).get("completion_tokens"),
                    "cached": ((usage or {}).get("prompt_tokens_details") or {}).get("cached_tokens"),
                })

    @staticmethod
    def usage_of(raw):
        text = raw.decode("utf-8", "replace").strip()
        if text.startswith("data:"):
            text = text[5:].strip()
        if '"usage"' not in text:
            return None
        try:
            record = json.loads(text)
        except ValueError:
            return None
        usage = record.get("usage") if isinstance(record, dict) else None
        return usage if isinstance(usage, dict) else None

    def between(self, start, end):
        with self.lock:
            calls = [r for r in self.records if start <= r["start"] and r["end"] <= end]

        def total(key):
            return sum(r[key] or 0 for r in calls)

        return {
            "calls": len(calls),
            "calls_without_usage": sum(1 for r in calls if r["input"] is None),
            "failed_calls": sum(1 for r in calls if r["status"] >= 400),
            "input_tokens": total("input"),
            "output_tokens": total("output"),
            "cached_tokens": total("cached"),
            "held_s": round(sum(r["held_s"] for r in calls), 3),
        }


# ── The scratch HOME ────────────────────────────────────────────────────────

def write_home(home, args, base_url):
    config = os.path.join(home, ".config", "chatty")
    for d in (config, os.path.join(home, ".local", "share"), os.path.join(home, ".cache"),
              os.path.join(home, ".local", "state"), os.path.join(home, "run")):
        os.makedirs(d, exist_ok=True)
    provider = {"name": "bench", "provider_type": "open_router", "base_url": base_url,
                "api_key": "no-key-required"}
    ptype = "open_router"
    models = []
    for arm in ARMS:
        identifier = args.model
        # The fake model routes its script by model name; a real server sees
        # the same model on both arms.
        if args.provider == "fake":
            identifier = "%s-%s" % (args.model, arm)
        model = {"id": arm, "name": identifier, "provider_type": ptype,
                 "model_identifier": identifier, "temperature": args.temperature}
        if args.think is not None:
            model["extra_params"] = {"think": args.think}
        models.append(model)
    execution = {
        "enabled": True, "approval_mode": "AutoApproveAll", "workspace_dir": None,
        "filesystem_read_enabled": True, "filesystem_write_enabled": True,
        "fetch_enabled": False, "git_enabled": True, "browser_enabled": False,
        "execute_code_enabled": False, "docker_code_execution_enabled": False,
        "docker_host": None, "timeout_seconds": 120, "max_output_bytes": 1048576,
        "network_isolation": False, "max_agent_turns": args.max_turns,
        "memory_enabled": False, "embedding_enabled": False,
        "hosted_conversations_enabled": False,
    }
    for name, value in (("providers.json", [provider]), ("models.json", models),
                        ("execution_settings.json", execution)):
        path = os.path.join(config, name)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(value, f, indent=2)
    with open(os.path.join(home, ".gitconfig"), "w") as f:
        f.write("[user]\n\temail = swarm-bench@chatty.invalid\n\tname = swarm-bench\n"
                "[commit]\n\tgpgsign = false\n[init]\n\tdefaultBranch = main\n")


# ── One run ─────────────────────────────────────────────────────────────────

def command(args, task, arm, workspace, usage_path):
    cmd = [args.chatty_tui, "--headless", "--auto-approve", "--workspace", workspace,
           "--model", arm, "--usage-file", usage_path, "--max-duration", args.max_duration]
    if arm == "swarm":
        cmd += ["--team", FAMILY_PRESET[task["family"]]]
    else:
        cmd += ["--max-agent-turns", str(args.max_turns)]
    return cmd + ["-m", task["prompt"]]


def wait_for_server(args):
    """Before a run: wait while the shared server is at its limit."""
    if not args.metrics_url:
        return 0.0
    start = time.time()
    noted = False
    while True:
        load = vllm_load(args.metrics_url)
        if load is None or load < args.throttle_max:
            return time.time() - start
        if not noted:
            log("server busy (%s requests); waiting" % int(load))
            noted = True
        time.sleep(5)


def run_one(args, task, arm, env, meter, scratch, out_dir):
    workspace = os.path.join(scratch, "%s-%s" % (task["name"], arm))
    if os.path.exists(workspace):
        shutil.rmtree(workspace)
    base = prepare_workspace(task, workspace)
    os.makedirs(out_dir, exist_ok=True)
    usage_path = os.path.join(out_dir, "usage.json")
    if os.path.exists(usage_path):
        os.remove(usage_path)
    cmd = command(args, task, arm, workspace, usage_path)
    waited = wait_for_server(args)
    started_at = now_iso()
    start = time.time()
    answer_path = os.path.join(out_dir, "answer.txt")
    with open(answer_path, "w") as out, open(os.path.join(out_dir, "stderr.log"), "w") as err:
        proc = subprocess.Popen(cmd, cwd=workspace, env=env, stdout=out, stderr=err,
                                start_new_session=True)
        try:
            code = proc.wait(timeout=args.run_timeout)
        except subprocess.TimeoutExpired:
            code = "timeout"
        # Workers live in the run's process group; nothing outlives the run.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(proc.pid, sig)
            except OSError:
                break
            time.sleep(2)
        if code == "timeout":
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
    end = time.time()
    usage = None
    if os.path.isfile(usage_path):
        try:
            usage = json.load(open(usage_path))
        except ValueError:
            usage = None
    verdict = subprocess.run(
        [sys.executable, os.path.join(HERE, "verify.py"), task["dir"], workspace, answer_path],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
    try:
        check = json.loads(verdict.stdout)
    except ValueError:
        check = {"pass": False, "reason": "verifier failed: %s" % verdict.stderr.strip()[-400:]}
    if task["family"] == "code-fix":
        try:
            diff = git(workspace, "diff", base, "--", ".", ":(exclude).chatty")
            diff += git(workspace, "status", "--porcelain", "--untracked-files=all", "--",
                        ".", ":(exclude).chatty")
        except RuntimeError as error:
            diff = str(error)
        with open(os.path.join(out_dir, "diff.patch"), "w") as f:
            f.write(diff)
    try:
        branches = [b.strip(" *+") for b in git(workspace, "branch", "--list").splitlines()]
    except RuntimeError:
        branches = []
    result = {
        "task": task["name"], "family": task["family"], "arm": arm,
        "preset": FAMILY_PRESET[task["family"]] if arm == "swarm" else None,
        "exit_code": code, "started_at": started_at,
        "wall_ms": int(round((end - start) * 1000)), "waited_before_s": round(waited, 1),
        "pass": bool(check.get("pass")), "check": check,
        "usage": usage, "meter": meter.between(start, end),
        "worker_branches": sorted(b for b in branches if b.startswith("sub-agent/")),
        "complete": True,
    }
    with open(os.path.join(out_dir, "result.json"), "w") as f:
        json.dump(result, f, indent=2, sort_keys=True)
    if not args.keep_work:
        shutil.rmtree(workspace, ignore_errors=True)
    return result


# ── Main ────────────────────────────────────────────────────────────────────

def parse_args(argv):
    p = argparse.ArgumentParser(prog="run.sh", description=__doc__.split("\n\n")[0])
    p.add_argument("--provider", required=True, choices=("openai-compat", "fake"),
                   help="openai-compat: a local vLLM (default %s); fake: the BI-0 fake model" % VLLM_URL)
    p.add_argument("--model", required=True)
    p.add_argument("--arm", default="both", choices=("single", "swarm", "both"))
    p.add_argument("--base-url", help="the provider's URL (required for --provider fake)")
    p.add_argument("--think", choices=("true", "false"))
    p.add_argument("--temperature", type=float, default=0.3)
    p.add_argument("--only", help="comma-separated task names (a pre-registered subset)")
    p.add_argument("--limit", type=int, help="the first N tasks of the interleaved order")
    p.add_argument("--out", default=DEFAULT_OUT)
    p.add_argument("--run-id")
    p.add_argument("--tasks", default=DEFAULT_TASKS)
    p.add_argument("--chatty-tui", required=True)
    p.add_argument("--max-turns", type=int, default=40, help="the single arm's turn budget")
    p.add_argument("--max-duration", default="30m")
    p.add_argument("--run-timeout", type=int, default=2400, help="hard kill after N seconds")
    p.add_argument("--metrics-url", help="vLLM /metrics for the throttle "
                   "(default: derived from --base-url for openai-compat)")
    p.add_argument("--throttle-max", type=int, default=2)
    p.add_argument("--keep-work", action="store_true", help="keep the scratch workspaces")
    args = p.parse_args(argv)
    if args.provider == "fake" and not args.base_url:
        die("--provider fake needs --base-url: the fake model is started by the "
            "swarm_bench_dry_run test (cargo test -p chatty-tui --test swarm_bench)")
    if args.provider == "openai-compat":
        args.base_url = (args.base_url or VLLM_URL).rstrip("/")
        if not args.base_url.endswith("/v1"):
            args.base_url += "/v1"
        if args.metrics_url is None:
            args.metrics_url = args.base_url[:-3] + "/metrics"
    return args


def main(argv):
    args = parse_args(argv)
    only = [x.strip() for x in args.only.split(",")] if args.only else None
    tasks = load_tasks(args.tasks, only)
    if args.limit:
        tasks = tasks[:args.limit]
    arms = ARMS if args.arm == "both" else (args.arm,)
    run_id = args.run_id or "%s-%s" % (time.strftime("%Y%m%d-%H%M%S"),
                                       re.sub(r"[^\w.-]+", "_", args.model))
    run_dir = os.path.join(args.out, run_id)
    os.makedirs(os.path.join(run_dir, "runs"), exist_ok=True)

    meta = {
        "schema": 1, "run_id": run_id, "provider": args.provider, "model": args.model,
        "think": args.think, "temperature": args.temperature,
        "max_turns_single": args.max_turns, "max_duration": args.max_duration,
        "family_preset": FAMILY_PRESET,
        "tasks": [t["name"] for t in tasks], "arms": list(arms),
        "prereg_sha256": sha256_file(PREREG) if os.path.isfile(PREREG) else None,
        "tasks_sha256": sha256_tree(args.tasks),
        "chatty_tui": args.chatty_tui,
    }
    try:
        meta["repo_head"] = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                           stdout=subprocess.PIPE, universal_newlines=True).stdout.strip()
        meta["chatty_tui_version"] = subprocess.run([args.chatty_tui, "--version"], stdout=subprocess.PIPE,
                                                    universal_newlines=True).stdout.strip()
    except OSError:
        pass
    meta_path = os.path.join(run_dir, "meta.json")
    if os.path.isfile(meta_path):
        old = json.load(open(meta_path))
        for key in ("provider", "model", "think", "tasks_sha256", "prereg_sha256"):
            if old.get(key) != meta.get(key):
                die("%s exists with %s=%r; use another --run-id" % (run_dir, key, old.get(key)))
        meta["started_at"] = old.get("started_at")
        meta["tasks"] = sorted(set(old.get("tasks", [])) | set(meta["tasks"]),
                               key=lambda n: [t["name"] for t in load_tasks(args.tasks, None)].index(n))
        meta["arms"] = sorted(set(old.get("arms", [])) | set(arms))
    meta.setdefault("started_at", now_iso())
    meta["started_at"] = meta["started_at"] or now_iso()
    with open(meta_path, "w") as f:
        json.dump(meta, f, indent=2, sort_keys=True)

    meter = Meter(args.base_url, metrics_url=args.metrics_url, throttle_max=args.throttle_max)
    scratch = tempfile.mkdtemp(prefix="swarm-bench-")
    home = os.path.join(scratch, "home")
    write_home(home, args, meter.url)
    env = dict(os.environ)
    env.update({
        "HOME": home,
        "XDG_CONFIG_HOME": os.path.join(home, ".config"),
        "XDG_DATA_HOME": os.path.join(home, ".local", "share"),
        "XDG_CACHE_HOME": os.path.join(home, ".cache"),
        "XDG_STATE_HOME": os.path.join(home, ".local", "state"),
        "XDG_RUNTIME_DIR": os.path.join(home, "run"),
        "PYTHONDONTWRITEBYTECODE": "1",
    })
    for key in list(env):
        if key.endswith("_API_KEY"):
            env.pop(key)
    try:
        for index, task in enumerate(tasks):
            # Counterbalanced order: even positions run single first.
            order = arms if index % 2 == 0 else tuple(reversed(arms))
            for arm in order:
                out_dir = os.path.join(run_dir, "runs", task["name"], arm)
                done = os.path.join(out_dir, "result.json")
                if os.path.isfile(done) and json.load(open(done)).get("complete"):
                    log("%s/%s: done already, skipped" % (task["name"], arm))
                    continue
                log("%s/%s: running" % (task["name"], arm))
                result = run_one(args, task, arm, env, meter, scratch, out_dir)
                log("%s/%s: %s in %.0f s, %d calls, %d+%d tokens (%s)" % (
                    task["name"], arm, "PASS" if result["pass"] else "fail",
                    result["wall_ms"] / 1000.0, result["meter"]["calls"],
                    result["meter"]["input_tokens"], result["meter"]["output_tokens"],
                    result["check"].get("reason", "")[:80]))
    finally:
        meta["finished_at"] = now_iso()
        with open(meta_path, "w") as f:
            json.dump(meta, f, indent=2, sort_keys=True)
        if not args.keep_work:
            shutil.rmtree(scratch, ignore_errors=True)
    print(run_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
