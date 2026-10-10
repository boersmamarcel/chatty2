#!/usr/bin/env python3
"""AGE-862 gate analysis (prereg §5-6): analyze.py <candidate> [--baseline c000-baseline]

Per task, the mean reward over each arm's reps; difference in points; 95 % paired bootstrap
over tasks (10 000 resamples, random.Random(862)). Test set: gates 1 and 3; EV-7: gate 2.
Writes heldout/<candidate>/gate.json. Gates 4-6 are review steps, not computed here.
"""
from __future__ import annotations

import json
import random
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mh  # noqa: E402


def per_task(cand: str, set_name: str) -> tuple[dict, int, float]:
    sc = mh.load_scores(cand, set_name)
    acc: dict = {}
    tokens, solved = 0, 0.0
    for k, run in sc["runs"].items():
        if not k.startswith(set_name + "-r"):
            continue
        tokens += run["tokens"]
        solved += run["solved"]
        for t, v in run["tasks"].items():
            acc.setdefault(t, []).append(v["reward"])
    return {t: statistics.mean(v) for t, v in acc.items()}, tokens, solved


def boot(a: dict, b: dict) -> dict:
    common = sorted(set(a) & set(b))
    d = [100 * (b[t] - a[t]) for t in common]
    rng = random.Random(862)
    means = sorted(statistics.mean(rng.choice(d) for _ in d) for _ in range(10_000))
    return {"n": len(common), "diff": statistics.mean(d), "ci95": [means[249], means[9749]],
            "base": 100 * statistics.mean(a[t] for t in common), "cand": 100 * statistics.mean(b[t] for t in common)}


def main() -> int:
    cand = sys.argv[1]
    base = sys.argv[sys.argv.index("--baseline") + 1] if "--baseline" in sys.argv else mh.BASE
    out: dict = {"candidate": cand, "baseline": base}
    ta, tok_a, sol_a = per_task(base, "test")
    tb, tok_b, sol_b = per_task(cand, "test")
    if ta and tb:
        g1 = boot(ta, tb)
        tps_a, tps_b = tok_a / max(sol_a, 1e-9), tok_b / max(sol_b, 1e-9)
        out["test"] = g1
        out["gate1_accuracy"] = g1["diff"] >= 5 and g1["ci95"][0] > 0
        out["tokens_per_solved"] = {"base": tps_a, "cand": tps_b, "ratio": tps_b / tps_a}
        out["gate3_cost"] = tps_b / tps_a <= 1.2
    ea, _, _ = per_task(base, "ev7")
    eb, _, _ = per_task(cand, "ev7")
    if ea and eb:
        g2 = boot(ea, eb)
        out["ev7"] = g2
        out["gate2_no_harm"] = g2["ci95"][0] >= -5
    p = mh.HELD / cand / "gate.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(out, indent=1))
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
