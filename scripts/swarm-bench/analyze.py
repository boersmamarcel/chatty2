#!/usr/bin/env python3
"""EV-7 (AGE-826) pre-registered analysis: swarm vs single, k replicates per task.

    analyze.py <run-dir> [<run-dir> ...] [--out report.md] [--json numbers.json]
    analyze.py --selftest

Run dirs are bench.py outputs (meta.json + runs/<task>/<arm>/result.json); the
replicate index comes from result.json's `rep`. Needs numpy, pandas, statsmodels
(python3.12 venv: /media/marcel/data/rust/swarm-results/ev7-venv/bin/python).

Primary: logistic mixed model on sub-parts, `part_pass ~ arm` with a random
intercept per task (statsmodels BinomialBayesMixedGLM, variational Bayes;
reported as log-odds ratio swarm vs single with a 95 % interval = mean +- 1.96
posterior SD, and as the implied difference in pass probability in points at the
average task). Sensitivity: logistic GEE clustered on task (robust SE).
Secondary: paired task-mean score difference (swarm - single), bootstrap over
tasks (10,000 resamples, seed 826), compared with the smallest effect of
interest, 25 points. Cost: tokens (input+output, every agent) per solved
sub-part, per arm. Every finished run counts, none is dropped for its outcome.
"""
import argparse
import glob
import json
import math
import os
import random
import sys

SEI = 0.25          # smallest effect of interest, score points as a fraction
BOOT = 10000
BOOT_SEED = 826


def load(dirs):
    rows, runs = [], []
    for d in dirs:
        for path in sorted(glob.glob(os.path.join(d, "runs", "*", "*", "result.json"))):
            r = json.load(open(path))
            if not r.get("complete"):
                continue
            parts = (r.get("check") or {}).get("parts") or {}
            m = r.get("meter") or {}
            tok = (m.get("input_tokens") or 0) + (m.get("output_tokens") or 0)
            rep = r.get("rep") or 0
            runs.append({"task": r["task"], "arm": r["arm"], "rep": rep, "n": len(parts),
                         "passed": sum(1 for v in parts.values() if v), "tokens": tok,
                         "wall_s": r.get("wall_ms", 0) / 1000.0, "seed": r.get("seed")})
            for k, v in parts.items():
                rows.append({"task": r["task"], "arm": r["arm"], "rep": rep,
                             "part": k, "y": int(bool(v))})
    return rows, runs


def boot_ci(diffs):
    rng = random.Random(BOOT_SEED)
    n = len(diffs)
    means = sorted(sum(diffs[rng.randrange(n)] for _ in range(n)) / n for _ in range(BOOT))
    return means[int(0.025 * BOOT)], means[int(0.975 * BOOT) - 1]


def task_means(runs):
    acc = {}
    for r in runs:
        if r["n"]:
            acc.setdefault((r["task"], r["arm"]), []).append(r["passed"] / r["n"])
    return {k: sum(v) / len(v) for k, v in acc.items()}


def analyse(rows, runs):
    import pandas as pd
    import numpy as np
    import statsmodels.api as sm
    from statsmodels.genmod.bayes_mixed_glm import BinomialBayesMixedGLM
    out = {}
    df = pd.DataFrame(rows)
    df["swarm"] = (df["arm"] == "swarm").astype(int)
    out["n_runs"] = {a: sum(1 for r in runs if r["arm"] == a) for a in ("single", "swarm")}
    out["n_parts"] = len(df)
    out["part_rate"] = {a: float(df[df.arm == a].y.mean()) for a in ("single", "swarm")}
    # Primary: random-intercept logistic.
    model = BinomialBayesMixedGLM.from_formula("y ~ swarm", {"task": "0 + C(task)"}, df)
    fit = model.fit_vb()
    names = list(model.exog_names)
    i = names.index("swarm")
    beta, sd = float(fit.fe_mean[i]), float(fit.fe_sd[i])
    icpt = float(fit.fe_mean[names.index("Intercept")])
    p0 = 1 / (1 + math.exp(-icpt))
    pf = lambda b: 1 / (1 + math.exp(-(icpt + b))) - p0
    out["primary_glmm"] = {"log_or": beta, "sd": sd, "ci95": [beta - 1.96 * sd, beta + 1.96 * sd],
                           "or": math.exp(beta),
                           "diff_points_at_avg_task": 100 * pf(beta),
                           "diff_points_ci95": [100 * pf(beta - 1.96 * sd), 100 * pf(beta + 1.96 * sd)],
                           "excludes_zero": (beta - 1.96 * sd > 0) or (beta + 1.96 * sd < 0),
                           "task_sd_logit": float(math.exp(fit.vcp_mean[0]))}
    # Sensitivity: GEE clustered on task.
    codes = df["task"].astype("category").cat.codes
    gee = sm.GEE(df["y"], sm.add_constant(df[["swarm"]]), groups=codes,
                 family=sm.families.Binomial(), cov_struct=sm.cov_struct.Exchangeable()).fit()
    b, se = float(gee.params["swarm"]), float(gee.bse["swarm"])
    out["sensitivity_gee"] = {"log_or": b, "se": se, "ci95": [b - 1.96 * se, b + 1.96 * se],
                              "p": float(gee.pvalues["swarm"])}
    # Secondary: paired task-mean difference.
    tm = task_means(runs)
    tasks = sorted({t for t, _ in tm if (t, "single") in tm and (t, "swarm") in tm})
    diffs = [tm[(t, "swarm")] - tm[(t, "single")] for t in tasks]
    if diffs:
        lo, hi = boot_ci(diffs)
        d = sum(diffs) / len(diffs)
        out["paired"] = {"tasks": len(tasks), "single": sum(tm[(t, "single")] for t in tasks) / len(tasks),
                         "swarm": sum(tm[(t, "swarm")] for t in tasks) / len(tasks),
                         "diff_points": 100 * d, "ci95_points": [100 * lo, 100 * hi],
                         "ci_excludes_zero": lo > 0 or hi < 0,
                         "sei_points": 100 * SEI,
                         "rules_out_sei_gain": hi < SEI, "rules_out_sei_loss": lo > -SEI,
                         "per_task": {t: [tm[(t, "single")], tm[(t, "swarm")]] for t in tasks}}
    # Cost.
    cost = {}
    for a in ("single", "swarm"):
        rr = [r for r in runs if r["arm"] == a]
        solved = sum(r["passed"] for r in rr)
        tok = sum(r["tokens"] for r in rr)
        cost[a] = {"tokens": tok, "solved_parts": solved,
                   "tokens_per_solved_part": tok / solved if solved else None,
                   "mean_wall_min": sum(r["wall_s"] for r in rr) / len(rr) / 60 if rr else None}
    if cost["single"]["tokens_per_solved_part"] and cost["swarm"]["tokens_per_solved_part"]:
        cost["ratio_swarm_over_single"] = (cost["swarm"]["tokens_per_solved_part"]
                                           / cost["single"]["tokens_per_solved_part"])
    out["cost"] = cost
    # Shared randomness check: per (task, rep) both arms used one seed.
    bad = []
    by = {}
    for r in runs:
        by.setdefault((r["task"], r["rep"]), set()).add(r["seed"])
    out["seed_mismatches"] = [list(k) for k, v in by.items() if len(v) > 1]
    out["runs_without_seed"] = sum(1 for r in runs if r["seed"] is None)
    return out


def render(o):
    g, p, c = o["primary_glmm"], o.get("paired"), o["cost"]
    L = ["# EV-7 analysis (AGE-826)", "",
         "Runs: single %d, swarm %d; sub-part observations %d." % (o["n_runs"]["single"], o["n_runs"]["swarm"], o["n_parts"]),
         "Raw part pass rate: single %.1f %%, swarm %.1f %%." % (100 * o["part_rate"]["single"], 100 * o["part_rate"]["swarm"]),
         "", "## Primary: part pass ~ arm + (1 | task)",
         "log-OR (swarm vs single) %.3f, 95 %% interval [%.3f, %.3f], OR %.2f; implied difference %.1f points "
         "[%.1f, %.1f] at the average task; interval %s zero." % (
             g["log_or"], g["ci95"][0], g["ci95"][1], g["or"], g["diff_points_at_avg_task"],
             g["diff_points_ci95"][0], g["diff_points_ci95"][1], "excludes" if g["excludes_zero"] else "includes"),
         "Task SD (logit) %.2f. Sensitivity, GEE clustered on task: log-OR %.3f [%.3f, %.3f], p = %.3f." % (
             g["task_sd_logit"], o["sensitivity_gee"]["log_or"], *o["sensitivity_gee"]["ci95"], o["sensitivity_gee"]["p"])]
    if p:
        L += ["", "## Secondary: paired task-mean difference (%d tasks)" % p["tasks"],
              "single %.1f %%, swarm %.1f %%; swarm - single = %.1f points, bootstrap 95 %% CI [%.1f, %.1f]." % (
                  100 * p["single"], 100 * p["swarm"], p["diff_points"], *p["ci95_points"]),
              "Smallest effect of interest %.0f points: CI %s a %.0f-point gain, %s a %.0f-point loss." % (
                  p["sei_points"], "rules out" if p["rules_out_sei_gain"] else "does not rule out", p["sei_points"],
                  "rules out" if p["rules_out_sei_loss"] else "does not rule out", p["sei_points"])]
    L += ["", "## Cost"]
    for a in ("single", "swarm"):
        x = c[a]
        L.append("- %s: %d tokens, %d solved parts, %s tokens per solved part, mean wall %.1f min" % (
            a, x["tokens"], x["solved_parts"],
            "%.0f" % x["tokens_per_solved_part"] if x["tokens_per_solved_part"] else "n/a", x["mean_wall_min"] or 0))
    if "ratio_swarm_over_single" in c:
        L.append("- swarm / single tokens per solved part: %.2f" % c["ratio_swarm_over_single"])
    L += ["", "Seed check: %d (task, rep) pairs with differing seeds across arms, %d runs without a seed." % (
        len(o["seed_mismatches"]), o["runs_without_seed"])]
    return "\n".join(L) + "\n"


def selftest():
    import tempfile
    rng = random.Random(1)
    d = tempfile.mkdtemp()
    for rep in (1, 2, 3):
        for ti in range(15):
            t = "t%02d" % ti
            base = rng.uniform(-1.5, 1.5)
            for arm, shift in (("single", 0.0), ("swarm", 1.0)):
                n = rng.randint(5, 9)
                parts = {"Q%d" % i: rng.random() < 1 / (1 + math.exp(-(base + shift))) for i in range(n)}
                path = os.path.join(d, "runs", t, arm)
                os.makedirs(path, exist_ok=True)
                json.dump({"task": t, "arm": arm, "rep": rep, "seed": 1000 + ti * 10 + rep, "complete": True,
                           "wall_ms": 1e6, "check": {"parts": parts},
                           "meter": {"input_tokens": 100000 * (2 if arm == "swarm" else 1), "output_tokens": 5000}},
                          open(os.path.join(path, "result.json"), "w"))
        # one dir per rep to exercise multi-dir loading
        os.rename(os.path.join(d, "runs"), os.path.join(d, "r%d" % rep))
        os.makedirs(os.path.join(d, "runs"))
    dirs = []
    for rep in (1, 2, 3):
        rd = os.path.join(d, "rd%d" % rep)
        os.makedirs(rd)
        os.rename(os.path.join(d, "r%d" % rep), os.path.join(rd, "runs"))
        dirs.append(rd)
    rows, runs = load(dirs)
    o = analyse(rows, runs)
    print(render(o))
    assert o["n_runs"] == {"single": 45, "swarm": 45}, o["n_runs"]
    assert o["primary_glmm"]["log_or"] > 0.4 and o["primary_glmm"]["excludes_zero"], o["primary_glmm"]
    assert not o["seed_mismatches"]
    print("selftest ok")


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("dirs", nargs="*")
    ap.add_argument("--out")
    ap.add_argument("--json")
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args(argv)
    if a.selftest:
        return selftest()
    rows, runs = load(a.dirs)
    if not rows:
        sys.exit("no finished runs found")
    o = analyse(rows, runs)
    text = render(o)
    print(text)
    if a.out:
        open(a.out, "w").write(text)
    if a.json:
        json.dump(o, open(a.json, "w"), indent=2, sort_keys=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
