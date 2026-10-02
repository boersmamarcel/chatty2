#!/usr/bin/env python3
"""The swarm-vs-single benchmark's report generator (EV-3, AGE-670).

    report.py <run-dir> [<run-dir> …] --out docs/research/swarm-vs-single-<date>.md
              [--json numbers.json] [--date YYYY-MM-DD]

Reads bench.py's results and writes the paired analysis the
pre-registration (docs/research/swarm-vs-single-prereg.md) fixes:

- primary: success rate per arm (Wilson 95 % CI), the paired difference
  swarm − single with a paired-bootstrap 95 % CI, and the exact McNemar
  test on the discordant pairs;
- secondary: tokens (input + output, counted at the wire, workers included)
  per solved task, and wall time, each with a paired geometric-mean ratio
  and its bootstrap CI;
- per family (descriptive only, not powered), failure reasons, and the
  per-task table.

A pair counts once both arms have a finished run. Every finished run counts:
a timeout, a crash or a missing deliverable is that arm's failure.

Written for Python 3.6+.
"""

import argparse
import datetime
import json
import math
import os
import random
import sys

SEED = 670
BOOT = 10000
ALPHA = 0.05
# Decision thresholds, fixed in the pre-registration.
COST_RATIO_LIMIT = 2.0
FAMILY_ORDER = ("data-audit", "code-fix", "research-write")


def load(run_dirs):
    runs, metas = {}, []
    for run_dir in run_dirs:
        meta_path = os.path.join(run_dir, "meta.json")
        if not os.path.isfile(meta_path):
            sys.exit("report.py: %s has no meta.json" % run_dir)
        metas.append(json.load(open(meta_path)))
        base = os.path.join(run_dir, "runs")
        for task in sorted(os.listdir(base)):
            for arm in ("single", "swarm"):
                path = os.path.join(base, task, arm, "result.json")
                if os.path.isfile(path):
                    r = json.load(open(path))
                    if r.get("complete"):
                        runs[(task, arm)] = r
    return runs, metas


def tokens(r):
    m = r.get("meter") or {}
    if m.get("calls"):
        return (m.get("input_tokens") or 0) + (m.get("output_tokens") or 0)
    u = r.get("usage") or {}
    return (u.get("input_tokens") or 0) + (u.get("output_tokens") or 0)


def wilson(k, n, z=1.959964):
    if n == 0:
        return (None, None)
    p = k / float(n)
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def comb(n, k):
    return math.factorial(n) // (math.factorial(k) * math.factorial(n - k))


def mcnemar_exact(b, c):
    """Two-sided exact McNemar p: the binomial test on b of b + c discordant pairs."""
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    tail = sum(comb(n, i) for i in range(k + 1)) / float(2 ** n)
    return min(1.0, 2 * tail)


def percentile(sorted_values, q):
    if not sorted_values:
        return None
    i = q * (len(sorted_values) - 1)
    lo, hi = int(math.floor(i)), int(math.ceil(i))
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * (i - lo)


def bootstrap(pairs, stat):
    """Percentile 95 % CI of `stat` over paired resamples (fixed seed)."""
    if not pairs:
        return (None, None)
    rng = random.Random(SEED)
    n = len(pairs)
    values = []
    for _ in range(BOOT):
        sample = [pairs[rng.randrange(n)] for _ in range(n)]
        v = stat(sample)
        if v is not None:
            values.append(v)
    values.sort()
    return (percentile(values, ALPHA / 2), percentile(values, 1 - ALPHA / 2))


def diff_stat(sample):
    return sum(s for _, s in sample) / float(len(sample)) - sum(x for x, _ in sample) / float(len(sample))


def geo_ratio(sample):
    logs = [math.log(s / x) for x, s in sample if x > 0 and s > 0]
    return math.exp(sum(logs) / len(logs)) if logs else None


def median(values):
    v = sorted(values)
    return percentile(v, 0.5) if v else None


def arm_stats(rs):
    n = len(rs)
    solved = sum(1 for r in rs if r["pass"])
    toks = sum(tokens(r) for r in rs)
    walls = [r["wall_ms"] / 1000.0 for r in rs]
    lo, hi = wilson(solved, n)
    return {
        "n": n, "solved": solved, "rate": solved / float(n) if n else None,
        "rate_ci": [lo, hi], "tokens": toks,
        "tokens_per_task": toks / float(n) if n else None,
        "tokens_per_solved": toks / float(solved) if solved else None,
        "wall_median_s": median(walls), "wall_p95_s": percentile(sorted(walls), 0.95),
        "wall_total_s": sum(walls),
        "model_calls": sum((r.get("meter") or {}).get("calls", 0) for r in rs),
        "timeouts": sum(1 for r in rs if r["exit_code"] == "timeout"),
        "nonzero_exit": sum(1 for r in rs if r["exit_code"] not in (0, "timeout")),
        "server_errors": sum(1 for r in rs if (r.get("meter") or {}).get("failed_calls")),
        "handoff_invalid": sum(sum(((r.get("usage") or {}).get("handoff_invalid_by_role") or {}).values())
                               for r in rs),
    }


def analyse(runs):
    tasks = sorted(set(t for t, _ in runs))
    paired = [t for t in tasks if (t, "single") in runs and (t, "swarm") in runs]
    single = [runs[(t, "single")] for t in paired]
    swarm = [runs[(t, "swarm")] for t in paired]
    n = len(paired)
    out = {"n_pairs": n, "unpaired": sorted(set(tasks) - set(paired)),
           "stats": {"single": arm_stats(single), "swarm": arm_stats(swarm)}}
    succ = [(1.0 if a["pass"] else 0.0, 1.0 if b["pass"] else 0.0) for a, b in zip(single, swarm)]
    b = sum(1 for x, s in succ if s > x)  # swarm solved, single did not
    c = sum(1 for x, s in succ if x > s)  # single solved, swarm did not
    delta = diff_stat(succ) if n else None
    lo, hi = bootstrap(succ, diff_stat)
    p = mcnemar_exact(b, c)
    out["primary"] = {"delta": delta, "delta_ci": [lo, hi], "swarm_only": b, "single_only": c,
                      "both": sum(1 for x, s in succ if x and s),
                      "neither": sum(1 for x, s in succ if not x and not s), "mcnemar_p": p}
    tok = [(float(tokens(a)), float(tokens(s))) for a, s in zip(single, swarm)]
    wall = [(a["wall_ms"] / 1000.0, s["wall_ms"] / 1000.0) for a, s in zip(single, swarm)]
    st = out["stats"]
    cps = None
    if st["single"]["tokens_per_solved"] and st["swarm"]["tokens_per_solved"]:
        cps = st["swarm"]["tokens_per_solved"] / st["single"]["tokens_per_solved"]
    out["secondary"] = {
        "token_ratio": geo_ratio(tok) if n else None, "token_ratio_ci": list(bootstrap(tok, geo_ratio)),
        "tokens_per_solved_ratio": cps,
        "wall_ratio": geo_ratio(wall) if n else None, "wall_ratio_ci": list(bootstrap(wall, geo_ratio)),
    }
    fam = {}
    for f in FAMILY_ORDER:
        ts = [t for t in paired if runs[(t, "single")]["family"] == f]
        if ts:
            fam[f] = {"n": len(ts),
                      "single": arm_stats([runs[(t, "single")] for t in ts]),
                      "swarm": arm_stats([runs[(t, "swarm")] for t in ts])}
    out["families"] = fam
    out["tasks"] = [{"task": t, "family": runs[(t, "single")]["family"],
                     "single": brief(runs[(t, "single")]), "swarm": brief(runs[(t, "swarm")])}
                    for t in paired]
    out["verdict"], out["prior"], out["recommendation_rule"] = judge(out)
    return out


def brief(r):
    return {"pass": r["pass"], "wall_s": round(r["wall_ms"] / 1000.0, 1), "tokens": tokens(r),
            "calls": (r.get("meter") or {}).get("calls", 0), "exit": r["exit_code"],
            "reason": (r.get("check") or {}).get("reason", "")}


def judge(out):
    """The pre-registered decision rule and the comparison with the prior."""
    pr = out["primary"]
    if out["n_pairs"] == 0:
        return "NO_DATA", "no data", "no data"
    if pr["delta"] > 0 and pr["mcnemar_p"] < ALPHA:
        verdict = "SWARM_BETTER"
    elif pr["delta"] < 0 and pr["mcnemar_p"] < ALPHA:
        verdict = "SINGLE_BETTER"
    else:
        verdict = "NO_DETECTABLE_DIFFERENCE"
    ratio = out["secondary"]["tokens_per_solved_ratio"]
    costlier = ratio is None or ratio > 1.0
    if verdict == "SWARM_BETTER":
        prior = "contradicts the prior"
    elif pr["delta"] <= 0 and costlier:
        prior = "agrees with the prior"
    else:
        prior = "does not contradict the prior, but does not confirm it either"
    if verdict == "SWARM_BETTER" and ratio is not None and ratio <= COST_RATIO_LIMIT:
        rule = "swarm worth its cost on this task set (better, tokens per solved task at most %.0fx)" % COST_RATIO_LIMIT
    elif verdict == "SWARM_BETTER":
        rule = "swarm better but costs more than %.0fx the tokens per solved task" % COST_RATIO_LIMIT
    else:
        rule = "no evidence the swarm is worth its cost on this task set"
    return verdict, prior, rule


# ── Rendering ───────────────────────────────────────────────────────────────

def pct(x):
    return "—" if x is None else "%.1f%%" % (100 * x)


def pp(x):
    return "—" if x is None else "%+.1f pp" % (100 * x)


def num(x, fmt="%.2f"):
    return "—" if x is None else fmt % x


def kt(x):
    return "—" if x is None else "{:,.0f}".format(x)


def render(res, metas, date):
    st, pr, sec = res["stats"], res["primary"], res["secondary"]
    meta = metas[0] if metas else {}
    lines = []
    w = lines.append
    w("# Swarm vs single agent: paired benchmark (%s)" % date)
    w("")
    w("**When to read this:** you want to know whether a Chatty team (the frozen")
    w("presets `data-analysis`, `fix-and-verify`, `research-brief`) beats one")
    w("harness agent with the same tools on the EV-3 task set, and at what cost.")
    w("")
    w("Generated by `scripts/swarm-bench/report.py` from the results of")
    w("`scripts/swarm-bench/run.sh`; the protocol is fixed in")
    w("[`swarm-vs-single-prereg.md`](./swarm-vs-single-prereg.md).")
    w("")
    w("## Setup")
    w("")
    w("| Field | Value |")
    w("|---|---|")
    w("| Provider / model | %s / `%s` |" % (meta.get("provider"), meta.get("model")))
    w("| Thinking | %s |" % (meta.get("think") or "provider default"))
    w("| Pairs | %d (one run per task and arm) |" % res["n_pairs"])
    w("| Presets | %s |" % ", ".join("%s → `%s`" % (f, p) for f, p in sorted((meta.get("family_preset") or {}).items())))
    w("| Single arm | one agent, no tool profile, no team, %s turns |" % meta.get("max_turns_single"))
    w("| Run cap | %s per run |" % meta.get("max_duration"))
    w("| Build | `%s` (%s) |" % ((meta.get("repo_head") or "?")[:10], meta.get("chatty_tui_version") or "?"))
    w("| Pre-registration sha256 | `%s` |" % (meta.get("prereg_sha256") or "?"))
    w("| Task set sha256 | `%s` |" % (meta.get("tasks_sha256") or "?"))
    w("| Run ids | %s |" % ", ".join("`%s`" % m.get("run_id") for m in metas))
    if res["unpaired"]:
        w("| Unpaired (left out) | %s |" % ", ".join(res["unpaired"]))
    w("")
    w("## Primary: success rate")
    w("")
    w("| Arm | Solved | Rate | 95% CI (Wilson) |")
    w("|---|---|---|---|")
    for arm in ("single", "swarm"):
        s = st[arm]
        w("| %s | %d/%d | %s | %s – %s |" % (arm, s["solved"], s["n"], pct(s["rate"]),
                                              pct(s["rate_ci"][0]), pct(s["rate_ci"][1])))
    w("")
    w("Paired difference swarm − single: **%s** (95%% paired-bootstrap CI %s to %s)." % (
        pp(pr["delta"]), pp(pr["delta_ci"][0]), pp(pr["delta_ci"][1])))
    w("Discordant pairs: swarm only %d, single only %d (both %d, neither %d);" % (
        pr["swarm_only"], pr["single_only"], pr["both"], pr["neither"]))
    w("exact McNemar p = %s." % num(pr["mcnemar_p"], "%.3f"))
    w("")
    w("**Verdict: %s.** The result %s." % (res["verdict"], res["prior"]))
    w("Decision rule: %s." % res["recommendation_rule"])
    w("")
    w("## Secondary: cost and time")
    w("")
    w("Tokens are input + output at the wire, every agent of a team included.")
    w("")
    w("| Arm | Tokens total | Tokens per task | Tokens per solved task | Model calls | Median wall | p95 wall |")
    w("|---|---|---|---|---|---|---|")
    for arm in ("single", "swarm"):
        s = st[arm]
        w("| %s | %s | %s | %s | %d | %s s | %s s |" % (
            arm, kt(s["tokens"]), kt(s["tokens_per_task"]), kt(s["tokens_per_solved"]),
            s["model_calls"], num(s["wall_median_s"], "%.0f"), num(s["wall_p95_s"], "%.0f")))
    w("")
    w("- Tokens, swarm ÷ single per task (geometric mean): **%sx** (95%% CI %s – %s)." % (
        num(sec["token_ratio"]), num(sec["token_ratio_ci"][0]), num(sec["token_ratio_ci"][1])))
    w("- Tokens per solved task, swarm ÷ single: **%sx**." % num(sec["tokens_per_solved_ratio"]))
    w("- Wall time, swarm ÷ single per task (geometric mean): **%sx** (95%% CI %s – %s)." % (
        num(sec["wall_ratio"]), num(sec["wall_ratio_ci"][0]), num(sec["wall_ratio_ci"][1])))
    w("")
    w("## By family (descriptive, not powered)")
    w("")
    w("| Family | Pairs | Single solved | Swarm solved | Single tokens/task | Swarm tokens/task | Single median wall | Swarm median wall |")
    w("|---|---|---|---|---|---|---|---|")
    for f in FAMILY_ORDER:
        if f not in res["families"]:
            continue
        x = res["families"][f]
        w("| %s | %d | %d | %d | %s | %s | %s s | %s s |" % (
            f, x["n"], x["single"]["solved"], x["swarm"]["solved"],
            kt(x["single"]["tokens_per_task"]), kt(x["swarm"]["tokens_per_task"]),
            num(x["single"]["wall_median_s"], "%.0f"), num(x["swarm"]["wall_median_s"], "%.0f")))
    w("")
    w("## Run health")
    w("")
    w("| Arm | Timeouts | Non-zero exits | Runs with server errors | Invalid handoffs |")
    w("|---|---|---|---|---|")
    for arm in ("single", "swarm"):
        s = st[arm]
        w("| %s | %d | %d | %d | %d |" % (arm, s["timeouts"], s["nonzero_exit"], s["server_errors"],
                                         s["handoff_invalid"]))
    w("")
    w("## Per task")
    w("")
    w("| Task | Single | Swarm | Single tokens | Swarm tokens | Single wall | Swarm wall | Failure reason |")
    w("|---|---|---|---|---|---|---|---|")
    for t in res["tasks"]:
        a, s = t["single"], t["swarm"]
        reasons = []
        if not a["pass"]:
            reasons.append("single: " + a["reason"])
        if not s["pass"]:
            reasons.append("swarm: " + s["reason"])
        reason = "; ".join(reasons).replace("|", "/").replace("\n", " ")
        if len(reason) > 160:
            reason = reason[:157] + "…"
        w("| %s | %s | %s | %s | %s | %.0f s | %.0f s | %s |" % (
            t["task"], "pass" if a["pass"] else "fail", "pass" if s["pass"] else "fail",
            kt(a["tokens"]), kt(s["tokens"]), a["wall_s"], s["wall_s"], reason))
    w("")
    return "\n".join(lines)


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument("run_dirs", nargs="+")
    p.add_argument("--out", required=True)
    p.add_argument("--json")
    p.add_argument("--date", default=datetime.date.today().isoformat())
    args = p.parse_args(argv)
    runs, metas = load(args.run_dirs)
    res = analyse(runs)
    with open(args.out, "w") as f:
        f.write(render(res, metas, args.date))
    if args.json:
        with open(args.json, "w") as f:
            json.dump(res, f, indent=2, sort_keys=True)
    print("%s: %s, %d pairs, single %s/%s, swarm %s/%s" % (
        args.out, res["verdict"], res["n_pairs"], res["stats"]["single"]["solved"], res["n_pairs"],
        res["stats"]["swarm"]["solved"], res["n_pairs"]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
