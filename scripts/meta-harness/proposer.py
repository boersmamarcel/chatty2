#!/usr/bin/env python3
"""AGE-862 proposer driver: Claude Code (Opus, headless) writes k=2 candidates per iteration.

Resumable: the iteration counter is the set of `candidates/p<i>-*` dirs plus
`proposer/iter-<i>.done`. Never blocks the eval queue: it only adds validated candidate
dirs, and it waits (instead of piling up work) while 2 proposer candidates are unscored.
On a usage-limit error it sleeps until the reset (or 30 min) and retries.

  proposer.py [--max-iters 20] [--pilot 5]

Pilot rule (prereg §4): after iteration 5's candidates are scored, if no candidate's
search-r1 mean exceeds the baseline band's max, write proposer/STOP_PILOT and stop.
Abort: same check after iteration 10.
"""
from __future__ import annotations

import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mh  # noqa: E402

P = mh.R / "proposer"
P.mkdir(parents=True, exist_ok=True)
CLAUDE = os.environ.get("CLAUDE_BIN", "claude")
ALLOWED = ("Read Glob Grep Write Edit Bash(./mh:*) Bash(jq:*) Bash(grep:*) Bash(ls:*) Bash(cp:*) "
           "Bash(mkdir:*) Bash(head:*) Bash(tail:*) Bash(wc:*) Bash(cat:*) Bash(diff:*) Bash(sed:*) Bash(python3:*)")
DENIED = ("Read(//media/marcel/data/rust/swarm-results/age-862/tasks.json) "
          "Read(//media/marcel/data/rust/swarm-results/age-862/heldout/**) "
          "Read(//media/marcel/data/rust/swarm-results/age-862/jobs/**) "
          "Bash(harbor:*) Bash(docker:*) Bash(uv:*) Bash(curl:*) WebFetch WebSearch")


def log(msg: str) -> None:
    line = f"{dt.datetime.now().isoformat(timespec='seconds')} {msg}"
    print(line, flush=True)
    with open(P / "proposer.log", "a") as f:
        f.write(line + "\n")


def cands(prefix: str) -> list[str]:
    return sorted(c.name for c in mh.CANDS.iterdir() if c.name.startswith(prefix))


def unscored_props() -> list[str]:
    return [c for c in cands("p") if (mh.CANDS / c / "VALID").exists() and not mh.job_done(c, "search", 1)]


def beats_band() -> tuple[bool, list]:
    band = mh.baseline_band()
    if band is None:
        return False, []
    hits = [(c, mh.search_mean(c)) for c in cands("p") + cands("k-")
            if mh.search_mean(c) is not None and mh.search_mean(c) > band[1]]
    return bool(hits), hits


def harness_hashes() -> dict:
    import hashlib
    out = {}
    for f in sorted(mh.CANDS.glob("*/harness/**/*")):
        if f.is_file():
            out[str(f)] = hashlib.sha256(f.read_bytes()).hexdigest()
    return out


def snapshot(it: int) -> dict:
    subprocess.call(["tar", "-C", str(mh.CANDS), "-cf", str(P / f"snap-{it}.tar"),
                     *[c.name + "/harness" for c in mh.CANDS.iterdir() if (c / "harness").is_dir()]])
    return harness_hashes()


def restore_if_touched(it: int, before: dict) -> None:
    after = harness_hashes()
    touched = [f for f, h in before.items() if after.get(f) != h]
    if touched:
        log(f"iter {it}: VIOLATION, proposer changed existing candidates {touched[:5]}; restoring")
        subprocess.call(["tar", "-C", str(mh.CANDS), "-xf", str(P / f"snap-{it}.tar")])


def sleep_for_limit(text: str) -> None:
    m = re.search(r"resets?\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?", text, re.I)
    secs = 1800
    if m:
        h, mi = int(m.group(1)), int(m.group(2) or 0)
        if m.group(3):
            h = h % 12 + (12 if m.group(3).lower() == "pm" else 0)
        now = dt.datetime.now()
        t = now.replace(hour=h % 24, minute=mi, second=0, microsecond=0)
        if t <= now:
            t += dt.timedelta(days=1)
        secs = min(int((t - now).total_seconds()) + 120, 6 * 3600)
    log(f"usage limit; sleeping {secs // 60} min")
    time.sleep(secs)


def run_claude(it: int, feedback: str = "") -> str:
    prompt = (f"Proposer iteration {it} of the AGE-862 Meta-Harness search. Follow the "
              f"meta-harness-proposer skill (.claude/skills/meta-harness-proposer/SKILL.md) exactly: "
              f"write 2 new candidates with ids p{it}-1-<slug> and p{it}-2-<slug>, validate both, "
              f"then stop. {feedback}")
    while True:
        r = subprocess.run([CLAUDE, "-p", prompt, "--model", "opus", "--output-format", "json",
                            "--permission-mode", "acceptEdits",
                            "--allowedTools", *ALLOWED.split(), "--disallowedTools", *DENIED.split()],
                           cwd=mh.STORE, capture_output=True, text=True, timeout=3 * 3600)
        out = r.stdout + "\n" + r.stderr
        (P / f"iter-{it}.json").write_text(r.stdout)
        with open(P / f"iter-{it}.stderr", "a") as f:
            f.write(r.stderr)
        if re.search(r"usage limit|limit reached|rate.?limit|hit your limit|resets? (at )?\d", out, re.I) and \
                not cands(f"p{it}-"):
            sleep_for_limit(out)
            continue
        try:
            j = json.loads(r.stdout)
            log(f"iter {it}: claude rc={r.returncode} cost=${j.get('total_cost_usd')} turns={j.get('num_turns')} "
                f"error={j.get('is_error')}")
        except Exception:  # noqa: BLE001
            log(f"iter {it}: claude rc={r.returncode} (non-JSON output)")
        return out


def accept(it: int) -> list[str]:
    ok = []
    for c in cands(f"p{it}-"):
        errs = mh.validate(c)
        meta = mh.CANDS / c / "meta.json"
        if not errs and meta.exists() and json.loads(meta.read_text()).get("source") != "proposer":
            errs.append("meta.json source must be proposer")
        if errs:
            (mh.CANDS / c / "VALID").unlink(missing_ok=True)
            (mh.CANDS / c / "INVALID").write_text("\n".join(errs))
            log(f"iter {it}: {c} INVALID: {errs}")
        else:
            (mh.CANDS / c / "VALID").touch()
            ok.append(c)
    return ok


def main() -> int:
    max_iters = int(sys.argv[sys.argv.index("--max-iters") + 1]) if "--max-iters" in sys.argv else 20
    pilot = int(sys.argv[sys.argv.index("--pilot") + 1]) if "--pilot" in sys.argv else 5
    log(f"proposer driver started (max_iters={max_iters}, pilot={pilot})")
    while not mh.job_done(mh.BASE, "search", 1):
        time.sleep(300)
    it = 1
    while it <= max_iters:
        if (P / "STOP").exists() or (P / "STOP_PILOT").exists():
            log("stop file present; exiting")
            return 0
        if (P / f"iter-{it}.done").exists():
            it += 1
            continue
        for checkpoint in (pilot, 10):
            if it == checkpoint + 1:
                while any(c.startswith(tuple(f"p{i}-" for i in range(1, checkpoint + 1))) for c in unscored_props()) \
                        or mh.baseline_band() is None or not all(mh.job_done(mh.BASE, "search", r) for r in (1, 2, 3)):
                    time.sleep(300)
                hit, hits = beats_band()
                verdict = {"checkpoint": checkpoint, "band": mh.baseline_band(), "beats": hits,
                           "at": dt.datetime.now().isoformat(timespec="seconds")}
                (P / f"checkpoint-{checkpoint}.json").write_text(json.dumps(verdict, indent=1))
                log(f"checkpoint after iteration {checkpoint}: {verdict}")
                if not hit:
                    (P / "STOP_PILOT").write_text(json.dumps(verdict, indent=1))
                    log("no candidate beats the baseline band: proposer stopped (pilot/abort rule)")
                    return 0
        while len(unscored_props()) >= 2:
            time.sleep(300)
        before = snapshot(it)
        out = run_claude(it)
        restore_if_touched(it, before)
        ok = accept(it)
        if not ok:
            errs = "; ".join((mh.CANDS / c / "INVALID").read_text() for c in cands(f"p{it}-")
                             if (mh.CANDS / c / "INVALID").exists())
            for c in cands(f"p{it}-"):
                subprocess.call(["chmod", "-R", "u+w", str(mh.CANDS / c)])
                os.rename(mh.CANDS / c, mh.CANDS / ("x" + c))
            run_claude(it, feedback=f"A previous attempt failed validation: {errs[:1500]}. Fix that.")
            ok = accept(it)
        log(f"iter {it}: accepted {ok}")
        (P / f"iter-{it}.done").write_text(json.dumps(ok))
        del out
        it += 1
    log("proposer reached max iterations")
    return 0


if __name__ == "__main__":
    sys.exit(main())
