#!/usr/bin/env python3
"""The resume spike's report generator (RC-1, AGE-650).

    python3 scripts/resume-spike/report.py [RESULTS ...] [--out FILE] [--json FILE]

RESULTS are run directories (each holds meta.json) or directories above
them; the default is target/resume-spike. Runs with the same provider, model
and condition pool into one group. The report goes to
docs/research/resume-spike-<date>.md unless --out says otherwise.

Cost is computed here, from the usage facts the runs recorded:
- a priced model (the run's meta.json has prices): TokenPricing's formula,
  uncached input at the input rate, cache reads and writes at theirs (the
  input rate when unset), output at the output rate;
- an unpriced model: uncached input plus output tokens.
The first task of a pair is shared by both arms and is left out of both.

Kill criterion (spec §3.3), on each cold group: arm C's cost per solved task
at least 20 % lower, its wall-clock at most 10 % worse, its pass rate at most
5 points lower, over at least --min-pairs (20) valid pairs. Any metric within
3 points of its bound means re-run at 50 pairs, unless a metric already
misses its bound by more than 3 points (FAIL) or the group already has 50.

Written for Python 3.6+.
"""

import argparse
import datetime
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
INF = float("inf")

COST_BOUND = 20.0   # arm C's cost per solved task at least this % lower
WALL_BOUND = 10.0   # arm C's wall-clock at most this % worse
PASS_BOUND = -5.0   # arm C's pass rate at most this many points lower
NEAR = 3.0          # within this many points of a bound: re-run
RERUN_PAIRS = 50


def find_runs(paths):
    runs = []
    for path in paths:
        for dirpath, dirnames, filenames in os.walk(path):
            if "meta.json" in filenames:
                runs.append(dirpath)
                dirnames[:] = []
    return sorted(set(runs))


def load_run(run_dir):
    with open(os.path.join(run_dir, "meta.json")) as f:
        meta = json.load(f)
    pairs = []
    pairs_dir = os.path.join(run_dir, "pairs")
    if os.path.isdir(pairs_dir):
        for name in sorted(os.listdir(pairs_dir)):
            path = os.path.join(pairs_dir, name, "pair.json")
            if os.path.isfile(path):
                with open(path) as f:
                    pair = json.load(f)
                pair["run_id"] = meta["run_id"]
                pairs.append(pair)
    return meta, pairs


def cost(usage, pricing):
    """USD for a priced model, uncached input + output tokens otherwise."""
    if not usage:
        return None
    total_in = usage.get("input_tokens") or 0
    out = usage.get("output_tokens") or 0
    read = usage.get("cache_read_tokens") or 0
    write = usage.get("cache_write_tokens") or 0
    if not pricing:
        return float(total_in - read + out)
    uncached = total_in - read - write
    rate_read = pricing.get("cache_read")
    rate_write = pricing.get("cache_write")
    return (uncached * pricing["input"] + out * pricing["output"]
            + read * (pricing["input"] if rate_read is None else rate_read)
            + write * (pricing["input"] if rate_write is None else rate_write)) / 1e6


def arm_stats(pairs, arm, pricing):
    runs = [p["arms"][arm] for p in pairs]
    solved = sum(1 for r in runs if r.get("pass"))
    total = sum(cost(r.get("usage"), pricing) or 0.0 for r in runs)
    wall = sum(r.get("wall_ms") or 0 for r in runs) / 1000.0
    return {
        "pairs": len(runs),
        "solved": solved,
        "pass_rate": 100.0 * solved / len(runs) if runs else 0.0,
        "cost_total": total,
        "cost_per_solved": total / solved if solved else INF,
        "wall_total_s": wall,
        "wall_mean_s": wall / len(runs) if runs else 0.0,
    }


def judge(n, r, c, min_pairs):
    """The kill-criterion verdict for one cold group."""
    if r["cost_per_solved"] == INF and c["cost_per_solved"] == INF:
        cost_reduction = -INF
    elif r["cost_per_solved"] == INF:
        cost_reduction = 100.0
    elif c["cost_per_solved"] == INF:
        cost_reduction = -INF
    else:
        cost_reduction = 100.0 * (1.0 - c["cost_per_solved"] / r["cost_per_solved"])
    wall_worse = 100.0 * (c["wall_total_s"] / r["wall_total_s"] - 1.0) if r["wall_total_s"] else INF
    pass_delta = c["pass_rate"] - r["pass_rate"]
    criteria = [
        ("cost per solved task", lower(cost_reduction), ">= 20 % lower",
         cost_reduction - COST_BOUND),
        ("wall-clock", worse(wall_worse), "<= 10 % worse", WALL_BOUND - wall_worse),
        ("pass rate", "%s points" % signed(pass_delta), ">= -5 points", pass_delta - PASS_BOUND),
    ]
    rows = [{"criterion": name, "arm_c": value, "bound": bound, "margin": margin,
             "met": margin >= 0, "near": abs(margin) <= NEAR}
            for name, value, bound, margin in criteria]
    if n < min_pairs:
        verdict = "INSUFFICIENT PAIRS"
        why = "%d valid pairs, the criterion needs at least %d" % (n, min_pairs)
    elif any(row["margin"] < -NEAR for row in rows):
        verdict = "FAIL"
        why = "misses " + ", ".join(r["criterion"] for r in rows if not r["met"])
    elif any(row["near"] for row in rows) and n < RERUN_PAIRS:
        verdict = "RE-RUN AT 50 PAIRS"
        why = "within 3 points of the bound: " + ", ".join(r["criterion"] for r in rows if r["near"])
    elif all(row["met"] for row in rows):
        verdict = "PASS"
        why = "arm C meets all three bounds"
    else:
        verdict = "FAIL"
        why = "misses " + ", ".join(r["criterion"] for r in rows if not r["met"])
    return {"verdict": verdict, "why": why, "criteria": rows, "cost_reduction": cost_reduction,
            "wall_worse": wall_worse, "pass_delta": pass_delta}


def overall(groups):
    cold = [g for g in groups if g["condition"] == "cold" and g.get("judgement")]
    if not cold:
        return "NO COLD RESULTS", "the verdict is taken on the cold condition only"
    verdicts = [g["judgement"]["verdict"] for g in cold]
    if "FAIL" in verdicts:
        return "FAIL", "a cold group fails the kill criterion"
    for v in ("INSUFFICIENT PAIRS", "RE-RUN AT 50 PAIRS"):
        if v in verdicts:
            return v, "a cold group has not settled"
    return "PASS", "every cold group passes the kill criterion"


# ── formatting ──────────────────────────────────────────────────────────────

def lower(x):
    """A cost reduction in words."""
    if x in (INF, -INF):
        return "n/a"
    return "%.1f %% lower" % x if x >= 0 else "%.1f %% higher" % -x


def worse(x):
    """A wall-clock change in words."""
    if x in (INF, -INF):
        return "n/a"
    return "%.1f %% worse" % x if x >= 0 else "%.1f %% better" % -x


def signed(x):
    return "%+.1f" % x


def money(x, priced):
    if x is None:
        return "–"
    if x == INF:
        return "∞ (none solved)"
    return "$%.4f" % x if priced else "{:,.0f}".format(x)


def secs(ms):
    return "–" if ms is None else "%.1f" % (ms / 1000.0)


def prompt_eval_ms(run):
    pe = run.get("prompt_eval") or {}
    return pe.get("prompt_eval_ms")


def div(d):
    if not d:
        return "–"
    return "%d f +%d −%d" % (d["files"], d["insertions"], d["deletions"])


def group_section(g):
    priced = bool(g["pricing"])
    unit = "USD (TokenPricing)" if priced else "tokens (uncached input + output)"
    lines = ["## %s · %s · %s" % (g["provider"], g["model"], g["condition"]), ""]
    lines.append("Runs: %s. Cost unit: %s. %d pair(s) recorded, %d valid." % (
        ", ".join("`%s`" % r for r in g["runs"]), unit, len(g["all_pairs"]), len(g["pairs"])))
    if priced:
        p = g["pricing"]
        lines.append("Prices per million tokens: input $%s, output $%s, cache read %s, cache write %s." % (
            p["input"], p["output"],
            "$%s" % p["cache_read"] if p.get("cache_read") is not None else "input rate",
            "$%s" % p["cache_write"] if p.get("cache_write") is not None else "input rate"))
    lines.append("")
    has_pe = any(prompt_eval_ms(p["arms"].get(a, {})) is not None
                 for p in g["pairs"] for a in ("rebrief", "resume"))
    header = ["pair", "task", "kind", "R pass", "C pass", "R cost", "C cost", "R wall s", "C wall s"]
    if has_pe:
        header += ["R prompt-eval ms", "C prompt-eval ms"]
    header += ["R diff", "C diff", "R↔C"]
    lines.append("| " + " | ".join(header) + " |")
    lines.append("|" + " -- |" * len(header))
    for p in g["all_pairs"]:
        if not p.get("valid"):
            lines.append("| %s | %s | %s | invalid: %s |" % (
                p["pair"], p["task"], p["kind"], p.get("invalid_reason") or "?")
                + " |" * (len(header) - 4))
            continue
        r, c = p["arms"]["rebrief"], p["arms"]["resume"]
        row = [str(p["pair"]), p["task"], p["kind"], "✓" if r["pass"] else "✗",
               "✓" if c["pass"] else "✗", money(cost(r["usage"], g["pricing"]), priced),
               money(cost(c["usage"], g["pricing"]), priced), secs(r["wall_ms"]), secs(c["wall_ms"])]
        if has_pe:
            row += ["–" if prompt_eval_ms(x) is None else "%.0f" % prompt_eval_ms(x) for x in (r, c)]
        row += [div(r.get("divergence")), div(c.get("divergence")), div(p.get("arm_divergence"))]
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")
    lines.append("| arm | valid pairs | solved | pass rate | total cost | cost per solved task | mean wall s |")
    lines.append("| -- | -- | -- | -- | -- | -- | -- |")
    for label, key in (("R, re-brief", "rebrief"), ("C, cold resume", "resume")):
        s = g["stats"][key]
        lines.append("| %s | %d | %d | %.1f %% | %s | %s | %.1f |" % (
            label, s["pairs"], s["solved"], s["pass_rate"], money(s["cost_total"], priced),
            money(s["cost_per_solved"], priced), s["wall_mean_s"]))
    lines.append("")
    j = g["judgement"]
    r, c = g["stats"]["rebrief"], g["stats"]["resume"]
    ratio = ("%.3f" % (c["cost_per_solved"] / r["cost_per_solved"])
             if INF not in (c["cost_per_solved"], r["cost_per_solved"]) and r["cost_per_solved"] else "n/a")
    lines.append("Cost per solved task, C / R: **%s** (%s). Wall-clock, C vs R: %s. "
                 "Pass rate, C − R: %s points." % (ratio, lower(j["cost_reduction"]),
                                                    worse(j["wall_worse"]), signed(j["pass_delta"])))
    lines.append("")
    if g["condition"] == "cold":
        lines.append("Kill criterion on this group: **%s** (%s)." % (j["verdict"], j["why"]))
    else:
        lines.append("Warm condition: informational; the verdict is taken on the cold condition.")
    lines.append("")
    return lines


def build(paths, min_pairs, date):
    groups = {}
    for run_dir in find_runs(paths):
        meta, pairs = load_run(run_dir)
        key = (meta["provider"], meta["model"], meta["condition"])
        g = groups.setdefault(key, {"provider": key[0], "model": key[1], "condition": key[2],
                                    "pricing": meta.get("pricing"), "runs": [], "all_pairs": [],
                                    "template_sha256": set()})
        g["runs"].append(meta["run_id"])
        g["template_sha256"].add(meta.get("template_sha256"))
        g["all_pairs"].extend(pairs)
    out = []
    for key in sorted(groups):
        g = groups[key]
        g["pairs"] = [p for p in g["all_pairs"] if p.get("valid")
                      and "rebrief" in p["arms"] and "resume" in p["arms"]]
        g["stats"] = {arm: arm_stats(g["pairs"], arm, g["pricing"]) for arm in ("rebrief", "resume")}
        g["judgement"] = judge(len(g["pairs"]), g["stats"]["rebrief"], g["stats"]["resume"], min_pairs)
        g["template_sha256"] = sorted(s for s in g["template_sha256"] if s)
        out.append(g)
    verdict, why = overall(out)
    return {"date": date, "min_pairs": min_pairs, "verdict": verdict, "why": why, "groups": out}


def render(result):
    lines = ["# Resume spike: %s" % result["date"], "",
             "**When to read this:** You want the measured answer to whether a worker resumed "
             "with its own conversation (arm C) costs less per solved task than a fresh worker "
             "re-briefed from a template (arm R), and whether handles get built (RC-2, AGE-651).",
             "",
             "Generated by `scripts/resume-spike/report.py` from the runs listed per group. "
             "Method: `scripts/resume-spike/README.md`; prompts: "
             "[resume-spike-template.md](resume-spike-template.md).", "",
             "## Verdict", "",
             "**Verdict: %s** (%s)." % (result["verdict"], result["why"]), ""]
    if result["min_pairs"] != 20:
        lines += ["Minimum valid pairs for a verdict: %d (the spec's is 20)." % result["min_pairs"], ""]
    lines += ["| group | valid pairs | cost per solved task | wall-clock | pass rate | verdict |",
              "| -- | -- | -- | -- | -- | -- |"]
    for g in result["groups"]:
        j = g["judgement"]
        cells = ["%s%s" % (row["arm_c"], " ✓" if row["met"] else " ✗") for row in j["criteria"]]
        lines.append("| %s · %s · %s | %d | %s | %s | %s | %s |" % (
            g["provider"], g["model"], g["condition"], len(g["pairs"]), cells[0], cells[1], cells[2],
            j["verdict"] if g["condition"] == "cold" else "(warm: informational)"))
    lines += ["", "Bounds (spec §3.3): arm C's cost per solved task ≥ 20 % lower, wall-clock "
              "≤ 10 % worse, pass rate ≤ 5 points lower, over ≥ 20 pairs in the cold condition. "
              "A metric within 3 points of its bound means re-run at 50 pairs. Overall: FAIL if any "
              "cold group fails; PASS only if every cold group passes.", ""]
    for g in result["groups"]:
        lines += group_section(g)
    shas = sorted({s for g in result["groups"] for s in g["template_sha256"]})
    lines += ["## Provenance", "",
              "Template SHA-256: %s." % (", ".join("`%s`" % s for s in shas) or "unknown"),
              "The first task of each pair is shared by both arms and left out of both costs. "
              "Wall-clock is the follow-up run's own, without the cold wait.", ""]
    return "\n".join(lines)


def to_json(result):
    def clean(x):
        if isinstance(x, float) and x in (INF, -INF):
            return None
        if isinstance(x, dict):
            return {k: clean(v) for k, v in x.items() if k not in ("all_pairs", "pairs")}
        if isinstance(x, list):
            return [clean(v) for v in x]
        return x

    out = clean(result)
    for g, src in zip(out["groups"], result["groups"]):
        g["valid_pairs"] = len(src["pairs"])
    return out


def main(argv):
    p = argparse.ArgumentParser(prog="report.py", description=__doc__.split("\n\n")[0])
    p.add_argument("results", nargs="*", default=[os.path.join(ROOT, "target", "resume-spike")])
    p.add_argument("--out")
    p.add_argument("--json", help="also write the numbers as JSON here")
    p.add_argument("--min-pairs", type=int, default=20)
    p.add_argument("--date", default=datetime.date.today().isoformat())
    args = p.parse_args(argv)
    result = build(args.results, args.min_pairs, args.date)
    if not result["groups"]:
        sys.stderr.write("report.py: no runs (meta.json) under %s\n" % ", ".join(args.results))
        sys.exit(2)
    out = args.out or os.path.join(ROOT, "docs", "research", "resume-spike-%s.md" % args.date)
    with open(out, "w", encoding="utf-8") as f:
        f.write(render(result))
    if args.json:
        with open(args.json, "w") as f:
            json.dump(to_json(result), f, indent=2)
    print("%s: %s" % (out, result["verdict"]))


if __name__ == "__main__":
    main(sys.argv[1:])
