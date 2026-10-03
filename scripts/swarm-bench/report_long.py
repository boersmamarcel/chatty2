#!/usr/bin/env python3
"""The long multi-part benchmark's report generator (EV-7, AGE-826).

    report_long.py <run-dir> [<run-dir> …] --out docs/research/swarm-vs-single-long-<date>.md
                   [--json numbers.json] [--date YYYY-MM-DD] [--calibration]

Reads bench.py's results for the task set in `evals/swarm-long/` and writes
the paired analysis that docs/research/swarm-vs-single-long-prereg.md fixes:

- primary: the mean sub-part score per arm, the paired difference
  swarm − single with a paired-bootstrap 95 % CI, and the exact two-sided
  sign-flip permutation test on the per-task differences;
- key secondary: full-task pass (every sub-part), with the exact McNemar test;
- secondary: tokens per task and per solved sub-part, wall time (paired
  geometric-mean ratios with bootstrap CIs), context pressure (calls refused
  for length, runs whose largest prompt reached 90 % of the window) and
  invalid handoffs;
- failure tags per run, per family (descriptive), and the per-task table.

`--calibration` instead summarises single-arm runs (several run dirs = the
replicates) and applies the calibration rule of
docs/research/swarm-vs-single-long-calibration.md.

Written for Python 3.6+.
"""

import argparse
import datetime
import itertools
import json
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import report as base  # noqa: E402

ALPHA = 0.05
COST_RATIO_LIMIT = 2.0
CONTEXT_WINDOW = 32768
SATURATED = 0.9
# The calibration band (fixed in the calibration protocol before any run).
BAND = (0.20, 0.70)


def score(r):
    s = r.get("score")
    if s is None:
        s = (r.get("check") or {}).get("score", 1.0 if r.get("pass") else 0.0)
    return float(s)


def parts_total(r):
    return (r.get("check") or {}).get("parts_total") or 0


def load_runs(run_dirs):
    """{(task, arm): [result, …]} over every run dir (replicates in order)."""
    out, metas = {}, []
    for run_dir in run_dirs:
        meta_path = os.path.join(run_dir, "meta.json")
        if not os.path.isfile(meta_path):
            sys.exit("report_long.py: %s has no meta.json" % run_dir)
        metas.append(json.load(open(meta_path)))
        runs = os.path.join(run_dir, "runs")
        for task in sorted(os.listdir(runs)):
            for arm in ("single", "swarm"):
                path = os.path.join(runs, task, arm, "result.json")
                if os.path.isfile(path):
                    r = json.load(open(path))
                    if r.get("complete"):
                        out.setdefault((task, arm), []).append(r)
    return out, metas


def sign_flip_p(diffs):
    """Two-sided exact sign-flip permutation p of the mean paired difference
    (Monte Carlo with 200,000 draws, seed 826, above 20 pairs)."""
    n = len(diffs)
    if n == 0:
        return 1.0
    observed = abs(sum(diffs))
    eps = 1e-12
    if n <= 20:
        hits = 0
        total = 0
        for signs in itertools.product((1, -1), repeat=n):
            total += 1
            if abs(sum(s * d for s, d in zip(signs, diffs))) >= observed - eps:
                hits += 1
        return hits / float(total)
    rng = random.Random(826)
    draws = 200000
    hits = sum(1 for _ in range(draws)
               if abs(sum(d if rng.random() < 0.5 else -d for d in diffs)) >= observed - eps)
    return (hits + 1) / float(draws + 1)


def tags(r):
    """Failure tags of one run, from the harness's own records."""
    out = []
    m = r.get("meter") or {}
    check = r.get("check") or {}
    if r.get("exit_code") == "timeout":
        out.append("timeout")
    elif r.get("exit_code") not in (0, None):
        out.append("exit %s" % r.get("exit_code"))
    if m.get("context_errors"):
        out.append("context overflow (%d refused)" % m["context_errors"])
    if (m.get("max_input_tokens") or 0) >= SATURATED * CONTEXT_WINDOW:
        out.append("context saturated")
    if check.get("missing_answers"):
        out.append("skipped %d sub-part(s)" % len(check["missing_answers"]))
    if check.get("found_in") is None and check.get("parts_total") and "found_in" in check:
        out.append("no deliverable")
    handoffs = sum(((r.get("usage") or {}).get("handoff_invalid_by_role") or {}).values())
    if handoffs:
        out.append("invalid handoffs %d" % handoffs)
    if r.get("arm") == "swarm" and not (r.get("usage") or {}).get("delegated_by_agent"):
        out.append("no delegation")
    return out


def arm_stats(rs):
    n = len(rs)
    st = base.arm_stats(rs)
    solved_parts = sum(score(r) * parts_total(r) for r in rs)
    st.update({
        "score_mean": sum(score(r) for r in rs) / float(n) if n else None,
        "parts_solved": solved_parts,
        "parts_total": sum(parts_total(r) for r in rs),
        "tokens_per_part": st["tokens"] / solved_parts if solved_parts else None,
        "context_error_runs": sum(1 for r in rs if (r.get("meter") or {}).get("context_errors")),
        "saturated_runs": sum(1 for r in rs
                              if ((r.get("meter") or {}).get("max_input_tokens") or 0) >= SATURATED * CONTEXT_WINDOW),
    })
    return st


def analyse(by_key):
    runs = {k: v[0] for k, v in by_key.items()}
    tasks = sorted(set(t for t, _ in runs))
    paired = [t for t in tasks if (t, "single") in runs and (t, "swarm") in runs]
    single = [runs[(t, "single")] for t in paired]
    swarm = [runs[(t, "swarm")] for t in paired]
    n = len(paired)
    out = {"n_pairs": n, "unpaired": sorted(set(tasks) - set(paired)),
           "stats": {"single": arm_stats(single), "swarm": arm_stats(swarm)}}
    sc = [(score(a), score(s)) for a, s in zip(single, swarm)]
    diffs = [s - a for a, s in sc]
    lo, hi = base.bootstrap(sc, base.diff_stat)
    out["primary"] = {"delta": base.diff_stat(sc) if n else None, "delta_ci": [lo, hi],
                      "p": sign_flip_p(diffs), "swarm_higher": sum(1 for d in diffs if d > 0),
                      "single_higher": sum(1 for d in diffs if d < 0),
                      "ties": sum(1 for d in diffs if d == 0)}
    succ = [(1.0 if a["pass"] else 0.0, 1.0 if s["pass"] else 0.0) for a, s in zip(single, swarm)]
    b = sum(1 for x, s in succ if s > x)
    c = sum(1 for x, s in succ if x > s)
    flo, fhi = base.bootstrap(succ, base.diff_stat)
    out["full"] = {"delta": base.diff_stat(succ) if n else None, "delta_ci": [flo, fhi],
                   "swarm_only": b, "single_only": c, "mcnemar_p": base.mcnemar_exact(b, c),
                   "both": sum(1 for x, s in succ if x and s),
                   "neither": sum(1 for x, s in succ if not x and not s)}
    tok = [(float(base.tokens(a)), float(base.tokens(s))) for a, s in zip(single, swarm)]
    wall = [(a["wall_ms"] / 1000.0, s["wall_ms"] / 1000.0) for a, s in zip(single, swarm)]
    st = out["stats"]
    cpp = None
    if st["single"]["tokens_per_part"] and st["swarm"]["tokens_per_part"]:
        cpp = st["swarm"]["tokens_per_part"] / st["single"]["tokens_per_part"]
    out["secondary"] = {
        "token_ratio": base.geo_ratio(tok) if n else None,
        "token_ratio_ci": list(base.bootstrap(tok, base.geo_ratio)),
        "tokens_per_part_ratio": cpp,
        "wall_ratio": base.geo_ratio(wall) if n else None,
        "wall_ratio_ci": list(base.bootstrap(wall, base.geo_ratio)),
    }
    fam = {}
    for f in base.FAMILY_ORDER:
        ts = [t for t in paired if runs[(t, "single")]["family"] == f]
        if ts:
            fam[f] = {"n": len(ts), "single": arm_stats([runs[(t, "single")] for t in ts]),
                      "swarm": arm_stats([runs[(t, "swarm")] for t in ts])}
    out["families"] = fam
    out["tasks"] = [{"task": t, "family": runs[(t, "single")]["family"],
                     "single": row(runs[(t, "single")]), "swarm": row(runs[(t, "swarm")])}
                    for t in paired]
    out["verdict"], out["prior"], out["recommendation_rule"] = judge(out)
    return out


def row(r):
    m = r.get("meter") or {}
    return {"pass": r["pass"], "score": score(r), "parts": (r.get("check") or {}).get("parts", {}),
            "wall_s": round(r["wall_ms"] / 1000.0, 1), "tokens": base.tokens(r),
            "calls": m.get("calls", 0), "max_input": m.get("max_input_tokens"),
            "exit": r["exit_code"], "tags": tags(r)}


def judge(out):
    pr = out["primary"]
    if out["n_pairs"] == 0:
        return "NO_DATA", "no data", "no data"
    if pr["delta"] > 0 and pr["p"] < ALPHA:
        verdict = "SWARM_BETTER"
    elif pr["delta"] < 0 and pr["p"] < ALPHA:
        verdict = "SINGLE_BETTER"
    else:
        verdict = "NO_DETECTABLE_DIFFERENCE"
    ratio = out["secondary"]["tokens_per_part_ratio"]
    costlier = ratio is None or ratio > 1.0
    if verdict == "SWARM_BETTER":
        prior = "contradicts the prior"
    elif pr["delta"] <= 0 and costlier:
        prior = "agrees with the prior"
    else:
        prior = "does not contradict the prior, but does not confirm it either"
    if verdict == "SWARM_BETTER" and ratio is not None and ratio <= COST_RATIO_LIMIT:
        rule = ("swarm worth its cost on this task set (better, tokens per solved sub-part "
                "at most %.0fx)" % COST_RATIO_LIMIT)
    elif verdict == "SWARM_BETTER":
        rule = "swarm better but costs more than %.0fx the tokens per solved sub-part" % COST_RATIO_LIMIT
    else:
        rule = "no evidence the swarm is worth its cost on this task set"
    return verdict, prior, rule


# ── Calibration ─────────────────────────────────────────────────────────────

def calibrate(by_key):
    rows = []
    for (task, arm), rs in sorted(by_key.items()):
        if arm != "single":
            continue
        scores = [score(r) for r in rs]
        mean = sum(scores) / len(scores)
        full = sum(1 for r in rs if r["pass"])
        in_band = BAND[0] <= mean <= BAND[1]
        rescued = mean > BAND[1] and full <= len(rs) // 2
        rows.append({"task": task, "family": rs[0]["family"], "k": len(rs), "scores": scores,
                     "mean": mean, "full": full, "keep": in_band or rescued,
                     "why": "in band" if in_band else ("above band, full pass %d/%d" % (full, len(rs))
                                                       if rescued else
                                                       ("below band" if mean < BAND[0] else "above band")),
                     "wall_s": [round(r["wall_ms"] / 1000.0) for r in rs],
                     "tokens": [base.tokens(r) for r in rs],
                     "max_input": [(r.get("meter") or {}).get("max_input_tokens") for r in rs],
                     "tags": [tags(r) for r in rs]})
    return rows


def render_calibration(rows, metas):
    w = []
    w.append("| Task | Family | Scores (k=%d) | Mean | Full passes | Keep | Why | Wall (s) | Tokens | Largest prompt |"
             % max([r["k"] for r in rows] or [0]))
    w.append("|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        w.append("| %s | %s | %s | %.2f | %d/%d | %s | %s | %s | %s | %s |" % (
            r["task"], r["family"], ", ".join("%.2f" % s for s in r["scores"]), r["mean"], r["full"],
            r["k"], "**yes**" if r["keep"] else "no", r["why"], ", ".join(str(x) for x in r["wall_s"]),
            ", ".join(base.kt(x) for x in r["tokens"]), ", ".join(base.kt(x) for x in r["max_input"])))
    kept = [r for r in rows if r["keep"]]
    w.append("")
    w.append("Kept %d of %d. Run ids: %s." % (len(kept), len(rows),
                                             ", ".join("`%s`" % m.get("run_id") for m in metas)))
    return "\n".join(w)


# ── Rendering ───────────────────────────────────────────────────────────────

def render(res, metas, date):
    st, pr, full, sec = res["stats"], res["primary"], res["full"], res["secondary"]
    meta = metas[0] if metas else {}
    pct, pp, num, kt = base.pct, base.pp, base.num, base.kt
    lines = []
    w = lines.append
    w("## Setup")
    w("")
    w("| Field | Value |")
    w("|---|---|")
    w("| Provider / model | %s / `%s` |" % (meta.get("provider"), meta.get("model")))
    w("| Thinking | %s |" % (meta.get("think") or "provider default"))
    w("| Pairs | %d (one run per task and arm) |" % res["n_pairs"])
    w("| Single arm | one agent, no tool profile, no team, %s turns |" % meta.get("max_turns_single"))
    w("| Run cap | %s per run |" % meta.get("max_duration"))
    w("| Build | `%s` (%s) |" % ((meta.get("repo_head") or "?")[:10], meta.get("chatty_tui_version") or "?"))
    w("| Pre-registration sha256 | `%s` |" % (meta.get("prereg_sha256") or "?"))
    w("| Task set sha256 | `%s` |" % (meta.get("tasks_sha256") or "?"))
    w("| Run ids | %s |" % ", ".join("`%s`" % m.get("run_id") for m in metas))
    if res["unpaired"]:
        w("| Unpaired (left out) | %s |" % ", ".join(res["unpaired"]))
    w("")
    w("## Primary: sub-part score")
    w("")
    w("| Arm | Mean sub-part score | Sub-parts solved | Full tasks solved | Full-pass 95% CI (Wilson) |")
    w("|---|---|---|---|---|")
    for arm in ("single", "swarm"):
        s = st[arm]
        w("| %s | %s | %.0f/%d | %d/%d | %s – %s |" % (
            arm, pct(s["score_mean"]), s["parts_solved"], s["parts_total"], s["solved"], s["n"],
            pct(s["rate_ci"][0]), pct(s["rate_ci"][1])))
    w("")
    w("Paired difference in sub-part score, swarm − single: **%s** (95%% paired-bootstrap CI %s to %s);"
      % (pp(pr["delta"]), pp(pr["delta_ci"][0]), pp(pr["delta_ci"][1])))
    w("swarm higher on %d tasks, single higher on %d, tied on %d; exact sign-flip permutation p = %s."
      % (pr["swarm_higher"], pr["single_higher"], pr["ties"], num(pr["p"], "%.3f")))
    w("")
    w("Full-task pass, swarm − single: %s (95%% CI %s to %s); swarm only %d, single only %d, "
      "both %d, neither %d; exact McNemar p = %s." % (
          pp(full["delta"]), pp(full["delta_ci"][0]), pp(full["delta_ci"][1]), full["swarm_only"],
          full["single_only"], full["both"], full["neither"], num(full["mcnemar_p"], "%.3f")))
    w("")
    w("**Verdict: %s.** The result %s. Decision rule: %s." % (res["verdict"], res["prior"],
                                                              res["recommendation_rule"]))
    w("")
    w("## Secondary: cost, time, context")
    w("")
    w("| Arm | Tokens total | Tokens per task | Tokens per solved sub-part | Model calls | Median wall | p95 wall | Runs refused for length | Runs at ≥ 90 % of the window | Invalid handoffs | Timeouts |")
    w("|---|---|---|---|---|---|---|---|---|---|---|")
    for arm in ("single", "swarm"):
        s = st[arm]
        w("| %s | %s | %s | %s | %d | %s s | %s s | %d | %d | %d | %d |" % (
            arm, kt(s["tokens"]), kt(s["tokens_per_task"]), kt(s["tokens_per_part"]), s["model_calls"],
            num(s["wall_median_s"], "%.0f"), num(s["wall_p95_s"], "%.0f"), s["context_error_runs"],
            s["saturated_runs"], s["handoff_invalid"], s["timeouts"]))
    w("")
    w("- Tokens, swarm ÷ single per task (geometric mean): **%sx** (95%% CI %s – %s)." % (
        num(sec["token_ratio"]), num(sec["token_ratio_ci"][0]), num(sec["token_ratio_ci"][1])))
    w("- Tokens per solved sub-part, swarm ÷ single: **%sx**." % num(sec["tokens_per_part_ratio"]))
    w("- Wall time, swarm ÷ single per task (geometric mean): **%sx** (95%% CI %s – %s)." % (
        num(sec["wall_ratio"]), num(sec["wall_ratio_ci"][0]), num(sec["wall_ratio_ci"][1])))
    w("")
    w("## By family (descriptive, not powered)")
    w("")
    w("| Family | Pairs | Single score | Swarm score | Single full | Swarm full | Single tokens/task | Swarm tokens/task |")
    w("|---|---|---|---|---|---|---|---|")
    for f in base.FAMILY_ORDER:
        if f in res["families"]:
            x = res["families"][f]
            w("| %s | %d | %s | %s | %d | %d | %s | %s |" % (
                f, x["n"], pct(x["single"]["score_mean"]), pct(x["swarm"]["score_mean"]),
                x["single"]["solved"], x["swarm"]["solved"], kt(x["single"]["tokens_per_task"]),
                kt(x["swarm"]["tokens_per_task"])))
    w("")
    w("## Per task")
    w("")
    w("| Task | Single score | Swarm score | Single tokens | Swarm tokens | Single wall | Swarm wall | Single tags | Swarm tags |")
    w("|---|---|---|---|---|---|---|---|---|")
    for t in res["tasks"]:
        a, s = t["single"], t["swarm"]
        w("| %s | %.2f | %.2f | %s | %s | %.0f s | %.0f s | %s | %s |" % (
            t["task"], a["score"], s["score"], kt(a["tokens"]), kt(s["tokens"]), a["wall_s"], s["wall_s"],
            ", ".join(a["tags"]) or "—", ", ".join(s["tags"]) or "—"))
    w("")
    return "\n".join(lines)


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument("run_dirs", nargs="+")
    p.add_argument("--out", required=True)
    p.add_argument("--json")
    p.add_argument("--date", default=datetime.date.today().isoformat())
    p.add_argument("--calibration", action="store_true")
    args = p.parse_args(argv)
    by_key, metas = load_runs(args.run_dirs)
    if args.calibration:
        rows = calibrate(by_key)
        text, res = render_calibration(rows, metas), rows
        summary = "%d of %d kept" % (sum(1 for r in rows if r["keep"]), len(rows))
    else:
        res = analyse(by_key)
        text = render(res, metas, args.date)
        summary = "%s, %d pairs, score single %s swarm %s" % (
            res["verdict"], res["n_pairs"], base.pct(res["stats"]["single"]["score_mean"]),
            base.pct(res["stats"]["swarm"]["score_mean"]))
    with open(args.out, "w") as f:
        f.write(text + "\n")
    if args.json:
        with open(args.json, "w") as f:
            json.dump(res, f, indent=2, sort_keys=True)
    print("%s: %s" % (args.out, summary))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
