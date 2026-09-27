#!/usr/bin/env python3
"""The resume spike's runner (RC-1, AGE-650). Start it through run.sh.

Per pair: a first worker does the task and its conversation is saved; then
arm C (resume) restores that conversation in the same worktree and takes the
follow-up, and arm R (re-brief) takes the follow-up as a fresh worker briefed
from the frozen template (docs/research/resume-spike-template.md). Each arm's
result is judged by the task's own verifier. In the cold condition each arm
waits out the provider's prompt cache first.

Results go to <out>/<run-id>/: meta.json, and per pair pairs/<NN-task>/ with
pair.json, each run's stdout/stderr/usage file, the first run's conversation
and each arm's diff. report.py turns them into the report. Worktrees and the
throwaway HOME (which holds the API key) live in a scratch dir that is
deleted at the end, so the results directory is safe to push.

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
TEMPLATE = os.path.join(ROOT, "docs", "research", "resume-spike-template.md")
DEFAULT_TASKS = os.path.join(HERE, "tasks")
DEFAULT_OUT = os.path.join(ROOT, "target", "resume-spike")

# The fake provider's made-up prices, USD per million tokens, so a dry run
# exercises the priced path.
FAKE_PRICES = {"input": 1.0, "output": 4.0, "cache_read": 0.1, "cache_write": 1.25}
OPENROUTER_URL = "https://openrouter.ai/api/v1"
OLLAMA_URL = "http://localhost:11434"
SUMMARY_CAP = 4000
MODEL_ID = "spike"
GIT_ID = ["-c", "user.name=resume-spike", "-c", "user.email=resume-spike@chatty.invalid",
          "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]
ARMS = ("resume", "rebrief")


def die(message):
    sys.stderr.write("resume-spike: %s\n" % message)
    sys.exit(2)


def now_iso():
    return datetime.datetime.utcnow().replace(microsecond=0).isoformat() + "Z"


# ── The frozen prompts ──────────────────────────────────────────────────────

def load_prompts(path=TEMPLATE):
    """The two `text` blocks of the frozen template: (re-brief, resume)."""
    text = open(path, encoding="utf-8").read()
    blocks = {}
    for heading, key in (("## Arm R, re-brief", "rebrief"), ("## Arm C, cold resume", "resume")):
        at = text.find(heading)
        if at < 0:
            die("%s has no '%s' section" % (path, heading))
        match = re.search(r"```text\n(.*?)\n```", text[at:], re.S)
        if not match:
            die("%s: '%s' has no text block" % (path, heading))
        blocks[key] = match.group(1)
    sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
    return blocks, sha


def fill(template, **slots):
    for name, value in slots.items():
        template = template.replace("{%s}" % name, value)
    return template


def cap_summary(text):
    text = text.strip()
    if len(text) <= SUMMARY_CAP:
        return text or "(the first worker printed no answer)"
    return "[…] " + text[-SUMMARY_CAP:]


# ── Tasks ───────────────────────────────────────────────────────────────────

def load_tasks(directory):
    tasks = []
    for name in sorted(os.listdir(directory)):
        path = os.path.join(directory, name)
        spec = os.path.join(path, "task.json")
        if not os.path.isfile(spec):
            continue
        with open(spec, encoding="utf-8") as f:
            data = json.load(f)
        for key in ("title", "kind", "task", "follow_up"):
            if not data.get(key):
                die("%s: missing '%s'" % (spec, key))
        for part in ("repo", "verify.py"):
            if not os.path.exists(os.path.join(path, part)):
                die("%s: missing %s" % (path, part))
        data["name"] = name
        data["dir"] = path
        tasks.append(data)
    if not tasks:
        die("no tasks under %s" % directory)
    return tasks


# ── git ─────────────────────────────────────────────────────────────────────

def git(cwd, *args):
    out = subprocess.run(["git"] + GIT_ID + list(args), cwd=cwd, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, universal_newlines=True)
    if out.returncode != 0:
        raise RuntimeError("git %s failed in %s: %s" % (" ".join(args), cwd, out.stderr.strip()))
    return out.stdout.strip()


def snapshot(cwd, message):
    """Commit whatever the worker left uncommitted; return HEAD."""
    git(cwd, "add", "-A")
    if git(cwd, "status", "--porcelain"):
        git(cwd, "commit", "-q", "-m", message)
    return git(cwd, "rev-parse", "HEAD")


def log_between(cwd, start, end):
    lines = git(cwd, "log", "--reverse", "--oneline", "%s..%s" % (start, end))
    return lines or "(none)"


def shortstat(cwd, a, b):
    """Files changed, insertions and deletions between two commits."""
    stat = git(cwd, "diff", "--shortstat", a, b)
    numbers = {"files": 0, "insertions": 0, "deletions": 0}
    for count, word in re.findall(r"(\d+) (file|insertion|deletion)", stat):
        numbers[{"file": "files", "insertion": "insertions", "deletion": "deletions"}[word]] = int(count)
    return numbers


# ── The Ollama meter: prompt-eval time the usage file does not carry ────────

class _Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


class OllamaMeter(object):
    """A pass-through proxy in front of Ollama that records each /api/chat
    response's final record (prompt_eval_count, prompt_eval_duration, …).

    It forwards every line as it arrives: a buffering proxy stalls a
    streaming client (AGE-460)."""

    def __init__(self, upstream):
        parts = urlsplit(upstream)
        self.host = parts.hostname
        self.port = parts.port or (443 if parts.scheme == "https" else 80)
        self.https = parts.scheme == "https"
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
        self.url = "http://127.0.0.1:%d" % self.server.server_address[1]
        thread = threading.Thread(target=self.server.serve_forever)
        thread.daemon = True
        thread.start()

    def forward(self, handler):
        length = int(handler.headers.get("Content-Length") or 0)
        body = handler.rfile.read(length) if length else None
        cls = http.client.HTTPSConnection if self.https else http.client.HTTPConnection
        upstream = cls(self.host, self.port, timeout=3600)
        headers = {}
        if handler.headers.get("Content-Type"):
            headers["Content-Type"] = handler.headers["Content-Type"]
        if body is not None:
            headers["Content-Length"] = str(len(body))
        try:
            upstream.request(handler.command, handler.path, body=body, headers=headers)
            response = upstream.getresponse()
        except OSError as error:
            handler.send_error(502, "resume-spike meter: %s" % error)
            return
        handler.send_response(response.status)
        # Only the content type goes back, stripped of line breaks: the
        # client reads the body until the connection closes.
        content_type = re.sub(r"[\r\n]", "", response.getheader("Content-Type") or "")
        if content_type:
            handler.send_header("Content-Type", content_type)
        handler.send_header("Connection", "close")
        handler.end_headers()
        handler.close_connection = True
        while True:
            line = response.readline()
            if not line:
                break
            handler.wfile.write(line)
            handler.wfile.flush()
            if handler.path.startswith("/api/chat"):
                self.note(line)
        upstream.close()

    def note(self, line):
        try:
            record = json.loads(line.decode("utf-8"))
        except ValueError:
            return
        if not isinstance(record, dict) or not record.get("done"):
            return
        with self.lock:
            self.records.append({
                "at": time.time(),
                "prompt_eval_count": record.get("prompt_eval_count"),
                "prompt_eval_ns": record.get("prompt_eval_duration"),
                "eval_count": record.get("eval_count"),
                "eval_ns": record.get("eval_duration"),
                "load_ns": record.get("load_duration"),
            })

    def between(self, start, end):
        """The calls that finished in [start, end], summed."""
        with self.lock:
            calls = [r for r in self.records if start <= r["at"] <= end]

        def total(key):
            values = [r[key] for r in calls if r[key] is not None]
            return sum(values) if values else None

        def ms(key):
            ns = total(key)
            return None if ns is None else ns / 1e6

        return {
            "calls": len(calls),
            "prompt_eval_count": total("prompt_eval_count"),
            "prompt_eval_ms": ms("prompt_eval_ns"),
            "eval_count": total("eval_count"),
            "eval_ms": ms("eval_ns"),
            "load_ms": ms("load_ns"),
        }


def ollama_model_loaded(base_url, model):
    """Whether Ollama still holds `model` in memory (its prompt cache with
    it), or None when /api/ps does not answer."""
    try:
        with urllib.request.urlopen(base_url.rstrip("/") + "/api/ps", timeout=10) as r:
            data = json.loads(r.read().decode("utf-8"))
    except (OSError, ValueError):
        return None
    names = [m.get("name") or m.get("model") for m in data.get("models") or []]
    return any(n == model or (n or "").split(":")[0] == model for n in names)


# ── Provider setup ──────────────────────────────────────────────────────────

def openrouter_prices(model, base_url):
    url = (base_url or OPENROUTER_URL).rstrip("/") + "/models"
    try:
        with urllib.request.urlopen(url, timeout=30) as r:
            data = json.loads(r.read().decode("utf-8"))
    except (OSError, ValueError) as error:
        die("cannot read OpenRouter's prices from %s (%s); pass --prices" % (url, error))
    for entry in data.get("data") or []:
        if entry.get("id") == model:
            pricing = entry.get("pricing") or {}

            def per_million(key):
                value = pricing.get(key)
                return None if value in (None, "") else float(value) * 1e6

            prices = {"input": per_million("prompt"), "output": per_million("completion"),
                      "cache_read": per_million("input_cache_read"),
                      "cache_write": per_million("input_cache_write")}
            if prices["input"] is None or prices["output"] is None:
                die("OpenRouter lists no prices for %s; pass --prices" % model)
            return prices
    die("OpenRouter has no model %s; pass --prices or check the id" % model)


def parse_prices(text):
    parts = [p.strip() for p in text.split(",")]
    if len(parts) < 2 or len(parts) > 4:
        die("--prices takes IN,OUT[,CACHE_READ[,CACHE_WRITE]] in USD per million tokens")
    values = [float(p) for p in parts] + [None] * (4 - len(parts))
    return dict(zip(("input", "output", "cache_read", "cache_write"), values))


def write_home(home, args, provider_url, prices):
    config = os.path.join(home, ".config", "chatty")
    for d in (config, os.path.join(home, ".local", "share"), os.path.join(home, ".cache"),
              os.path.join(home, ".local", "state"), os.path.join(home, "run")):
        os.makedirs(d, exist_ok=True)
    if args.provider == "ollama":
        provider = {"name": "Ollama", "provider_type": "ollama", "base_url": provider_url}
    else:
        key = "fake" if args.provider == "fake" else os.environ.get("OPENROUTER_API_KEY")
        if not key:
            die("--provider openrouter needs OPENROUTER_API_KEY in the environment")
        provider = {"name": "OpenRouter", "provider_type": "open_router", "api_key": key}
        if provider_url:
            provider["base_url"] = provider_url
    model = {"id": MODEL_ID, "name": args.model, "provider_type": provider["provider_type"],
             "model_identifier": args.model}
    if prices:
        model["cost_per_million_input_tokens"] = prices["input"]
        model["cost_per_million_output_tokens"] = prices["output"]
        if prices.get("cache_read") is not None:
            model["cost_per_million_cache_read_tokens"] = prices["cache_read"]
        if prices.get("cache_write") is not None:
            model["cost_per_million_cache_write_tokens"] = prices["cache_write"]
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
    for name, value in (("providers.json", [provider]), ("models.json", [model]),
                        ("execution_settings.json", execution)):
        path = os.path.join(config, name)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(value, f, indent=2)
        os.chmod(path, 0o600)
    with open(os.path.join(home, ".gitconfig"), "w") as f:
        f.write("[user]\n\temail = resume-spike@chatty.invalid\n\tname = resume-spike\n"
                "[commit]\n\tgpgsign = false\n")


# ── One worker run ──────────────────────────────────────────────────────────

class Runner(object):
    def __init__(self, args, home, meter):
        self.args = args
        self.home = home
        self.meter = meter
        self.env = dict(os.environ)
        self.env.update({
            "HOME": home,
            "XDG_CONFIG_HOME": os.path.join(home, ".config"),
            "XDG_DATA_HOME": os.path.join(home, ".local", "share"),
            "XDG_CACHE_HOME": os.path.join(home, ".cache"),
            "XDG_STATE_HOME": os.path.join(home, ".local", "state"),
            "XDG_RUNTIME_DIR": os.path.join(home, "run"),
        })
        self.env.pop("OPENROUTER_API_KEY", None)

    def run(self, cwd, message, prefix, restore=None, save=None):
        args = self.args
        usage_path = prefix + ".usage.json"
        cmd = [args.chatty_tui, "--headless", "--auto-approve", "--tools", "coder",
               "--workspace", cwd, "--model", MODEL_ID, "--usage-file", usage_path,
               "--max-agent-turns", str(args.max_turns), "--max-duration", args.max_duration]
        if args.think is not None:
            cmd += ["--think", args.think]
        if restore:
            cmd += ["--restore", restore]
        if save:
            cmd += ["--save-conversation", save]
        cmd += ["-m", message]
        with open(prefix + ".prompt.txt", "w", encoding="utf-8") as f:
            f.write(message)
        started_at = now_iso()
        start = time.time()
        with open(prefix + ".stdout", "w") as out, open(prefix + ".stderr", "w") as err:
            try:
                code = subprocess.run(cmd, cwd=cwd, env=self.env, stdout=out, stderr=err,
                                      timeout=args.run_timeout).returncode
            except subprocess.TimeoutExpired:
                code = "timeout"
        end = time.time()
        usage = None
        if os.path.isfile(usage_path):
            with open(usage_path) as f:
                usage = json.load(f)
        result = {"exit_code": code, "started_at": started_at,
                  "wall_ms": int(round((end - start) * 1000)), "usage": usage}
        if self.meter:
            result["prompt_eval"] = self.meter.between(start, end)
        return result, end


def verify(task, cwd):
    """The task's verifier on the worktree: (passed, log)."""
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    # The verifier runs the worker's code: it gets no provider key.
    env.pop("OPENROUTER_API_KEY", None)
    try:
        out = subprocess.run([sys.executable, os.path.join(task["dir"], "verify.py"), cwd],
                             cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             universal_newlines=True, timeout=180)
        return out.returncode == 0, out.stdout
    except subprocess.TimeoutExpired:
        return False, "verifier timed out after 180 s\n"


# ── One pair ────────────────────────────────────────────────────────────────

def run_pair(index, task, args, prompts, runner, work_root, pairs_dir):
    name = "%02d-%s" % (index + 1, task["name"])
    pair_dir = os.path.join(pairs_dir, name)
    pair_path = os.path.join(pair_dir, "pair.json")
    if os.path.isfile(pair_path):
        print("pair %s: already recorded, skipped" % name)
        with open(pair_path) as f:
            return json.load(f)
    os.makedirs(pair_dir, exist_ok=True)
    work = os.path.join(work_root, name)
    repo = os.path.join(work, "first")
    shutil.copytree(os.path.join(task["dir"], "repo"), repo)
    git(repo, "init", "-q")
    with open(os.path.join(repo, ".git", "info", "exclude"), "a") as f:
        f.write("__pycache__/\n*.pyc\n")
    base = snapshot(repo, "base")
    pair = {"schema": 1, "pair": index + 1, "task": task["name"], "title": task["title"],
            "kind": task["kind"], "valid": False, "invalid_reason": None, "arms": {}}

    def record():
        with open(pair_path + ".tmp", "w") as f:
            json.dump(pair, f, indent=2)
        os.replace(pair_path + ".tmp", pair_path)

    conversation = os.path.join(pair_dir, "first.conversation.json")
    first, last_end = runner.run(repo, task["task"], os.path.join(pair_dir, "first"),
                                 save=conversation)
    pair["first"] = first
    end_head = git(repo, "rev-parse", "HEAD")
    result = snapshot(repo, "resume-spike: record the first task's result")
    pair["first"]["divergence"] = shortstat(repo, base, result)
    exit_ = (first.get("usage") or {}).get("exit")
    if first["exit_code"] != 0 or exit_ not in ("completed", "deadline"):
        pair["invalid_reason"] = "first run did not complete (exit %s, usage exit %s)" % (
            first["exit_code"], exit_)
    elif not os.path.isfile(conversation):
        pair["invalid_reason"] = "first run saved no conversation"
    if pair["invalid_reason"]:
        record()
        print("pair %s: invalid: %s" % (name, pair["invalid_reason"]))
        return pair
    with open(conversation) as f:
        pair["first"]["conversation_messages"] = len(json.load(f))
    pair["first"]["conversation_bytes"] = os.path.getsize(conversation)

    rebrief_dir = os.path.join(work, "rebrief")
    git(repo, "worktree", "add", "-q", "--detach", rebrief_dir, result)
    summary = open(os.path.join(pair_dir, "first.stdout")).read()
    messages = {
        "rebrief": fill(prompts["rebrief"], task=task["task"], summary=cap_summary(summary),
                        commits=log_between(repo, base, result), follow_up=task["follow_up"]),
        "resume": fill(prompts["resume"], commits_since=log_between(repo, end_head, result),
                       follow_up=task["follow_up"]),
    }
    finals = {}
    for arm in args.arms:
        cwd = repo if arm == "resume" else rebrief_dir
        waited = 0.0
        loaded = None
        if args.condition == "cold":
            time.sleep(args.cold_wait)
            waited = float(args.cold_wait)
        if args.provider == "ollama":
            loaded = ollama_model_loaded(args.provider_url, args.model)
        gap = time.time() - last_end
        run, last_end = runner.run(cwd, messages[arm], os.path.join(pair_dir, arm),
                                   restore=conversation if arm == "resume" else None)
        final = snapshot(cwd, "resume-spike: record arm %s" % arm)
        finals[arm] = final
        with open(os.path.join(pair_dir, arm + ".diff"), "w") as f:
            f.write(git(cwd, "diff", result, final) + "\n")
        passed, log = verify(task, cwd)
        with open(os.path.join(pair_dir, arm + ".verify.log"), "w") as f:
            f.write(log)
        run.update({"waited_s": waited, "gap_s": round(gap, 1), "model_loaded_at_start": loaded,
                    "pass": passed, "divergence": shortstat(repo, result, final)})
        pair["arms"][arm] = run
        print("pair %s: %s %s in %.1fs" % (name, arm, "PASS" if passed else "fail",
                                           run["wall_ms"] / 1000.0))
    if len(finals) == 2:
        pair["arm_divergence"] = shortstat(repo, finals["rebrief"], finals["resume"])
    missing = [a for a in args.arms if pair["arms"][a].get("usage") is None]
    if missing:
        pair["invalid_reason"] = "no usage file from arm(s) %s" % ", ".join(missing)
    else:
        pair["valid"] = True
    record()
    return pair


# ── Main ────────────────────────────────────────────────────────────────────

def parse_args(argv):
    p = argparse.ArgumentParser(prog="run.sh", description="Run the resume spike (RC-1, AGE-650).")
    p.add_argument("--provider", required=True, choices=("openrouter", "ollama", "fake"))
    p.add_argument("--model", required=True)
    p.add_argument("--pairs", required=True, type=int)
    p.add_argument("--condition", required=True, choices=("warm", "cold"))
    p.add_argument("--arm", choices=("rebrief", "resume", "handles"),
                   help="run only this arm (default: both)")
    p.add_argument("--base-url", help="the provider's URL (required for --provider fake)")
    p.add_argument("--cold-wait", type=float, default=360.0,
                   help="seconds before each cold follow-up: cache TTL + 60 s (default 360)")
    p.add_argument("--prices", help="IN,OUT[,CACHE_READ[,CACHE_WRITE]] USD per million tokens")
    p.add_argument("--out", default=DEFAULT_OUT)
    p.add_argument("--run-id")
    p.add_argument("--tasks", default=DEFAULT_TASKS)
    p.add_argument("--chatty-tui", required=True)
    p.add_argument("--max-turns", type=int, default=40)
    p.add_argument("--max-duration", default="20m")
    p.add_argument("--think", choices=("true", "false"))
    p.add_argument("--keep-work", action="store_true", help="keep the scratch worktrees")
    args = p.parse_args(argv)
    if args.arm == "handles":
        die("--arm handles needs handles (RC-3, RC-4), which are not built yet")
    if args.pairs < 1:
        die("--pairs must be at least 1")
    if args.provider == "fake" and not args.base_url:
        die("--provider fake needs --base-url: the fake model is started by the "
            "resume_spike_dry_run test (cargo test -p chatty-tui --test resume_spike)")
    args.arms = [args.arm] if args.arm else list(ARMS)
    args.run_timeout = 3 * 3600
    return args


def main(argv):
    args = parse_args(argv)
    prompts, template_sha = load_prompts()
    tasks = load_tasks(args.tasks)

    if args.provider == "ollama":
        args.provider_url = (args.base_url or OLLAMA_URL).rstrip("/")
        prices = parse_prices(args.prices) if args.prices else None
    elif args.provider == "fake":
        args.provider_url = args.base_url
        prices = parse_prices(args.prices) if args.prices else dict(FAKE_PRICES)
    else:
        args.provider_url = args.base_url
        prices = parse_prices(args.prices) if args.prices else openrouter_prices(args.model, args.base_url)

    slug = re.sub(r"[^A-Za-z0-9.]+", "-", args.model).strip("-")
    run_id = args.run_id or "%s-%s-%s-%s" % (args.provider, slug, args.condition,
                                            datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    run_dir = os.path.join(os.path.abspath(args.out), run_id)
    pairs_dir = os.path.join(run_dir, "pairs")
    os.makedirs(pairs_dir, exist_ok=True)
    meta_path = os.path.join(run_dir, "meta.json")
    meta = {
        "schema": 1, "run_id": run_id, "provider": args.provider, "model": args.model,
        "condition": args.condition, "arms": args.arms, "pairs": args.pairs,
        "cold_wait_s": args.cold_wait if args.condition == "cold" else 0,
        "base_url": args.provider_url, "pricing": prices, "template_sha256": template_sha,
        "tasks": [t["name"] for t in tasks], "max_turns": args.max_turns,
        "max_duration": args.max_duration, "think": args.think,
        "chatty_tui": args.chatty_tui, "chatty2_rev": None,
        "started_at": now_iso(), "finished_at": None,
    }
    try:
        meta["chatty2_rev"] = git(ROOT, "rev-parse", "HEAD")
    except (RuntimeError, OSError):
        pass
    if os.path.isfile(meta_path):
        with open(meta_path) as f:
            old = json.load(f)
        for key in ("provider", "model", "condition", "arms", "template_sha256"):
            if old.get(key) != meta[key]:
                die("%s exists with %s=%r; use another --run-id" % (run_dir, key, old.get(key)))
        meta["started_at"] = old.get("started_at")

    def save_meta():
        with open(meta_path, "w") as f:
            json.dump(meta, f, indent=2)

    save_meta()
    scratch = tempfile.mkdtemp(prefix="resume-spike-")
    home = os.path.join(scratch, "home")
    meter = OllamaMeter(args.provider_url) if args.provider == "ollama" else None
    write_home(home, args, meter.url if meter else args.provider_url, prices)
    runner = Runner(args, home, meter)
    print("resume spike: %s, %d pair(s), %s, arms %s -> %s" % (
        args.model, args.pairs, args.condition, "+".join(args.arms), run_dir))
    try:
        for index in range(args.pairs):
            task = tasks[index % len(tasks)]
            run_pair(index, task, args, prompts, runner, os.path.join(scratch, "work"), pairs_dir)
    finally:
        if args.keep_work:
            print("scratch kept: %s (its home/ holds the provider key)" % scratch)
        else:
            shutil.rmtree(scratch, ignore_errors=True)
    meta["finished_at"] = now_iso()
    save_meta()
    print("done: %s" % run_dir)


if __name__ == "__main__":
    main(sys.argv[1:])
