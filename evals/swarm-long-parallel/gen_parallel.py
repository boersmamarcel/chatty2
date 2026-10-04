#!/usr/bin/env python3
"""Generator for the EV-7 "parallel" task family p01..p06 (AGE-826 addendum).

Six tasks whose parts are independent and broad, so that a team can split
them: document lookups (p01, p02), seeded flaws in separate modules (p03, p04)
and per-file CSV computations (p05, p06). Deterministic (fixed seeds). Rewrites
only evals/swarm-long-parallel/tasks/. Each task: task.json, check.json,
workspace/, solution/report.md (the reference answers). Python 3.6, stdlib only.
"""
import csv
import json
import os
import random
import shutil
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "tasks")
FORMAT = ("Write report.md in the working directory. It must START with one line per question, in "
          "order, exactly `Qn: <answer>` (for example `Q1: 1234`), followed by your working notes (do "
          "not start any other line with `Qn:`; the last such line is the one that counts). End your "
          "final chat answer with the same Q lines.")
WORDS = ("the of and to in is for on with as by at from this that an be are or it its which has have "
         "was were been into over after before between during under report review note team service "
         "queue handled processed incident follow action owner meeting update change plan release "
         "customer region capacity budget quarter weekly daily summary draft final approved pending "
         "history context background process steady normal stable minor routine regular standard").split()


def rng(*parts):
    return random.Random(zlib.crc32("/".join(str(p) for p in parts).encode()))


def prose(r, n):
    words = [r.choice(WORDS) for _ in range(n)]
    out, i = [], 0
    while i < len(words):
        k = r.randint(8, 16)
        out.append(" ".join(words[i:i + k]).capitalize() + ".")
        i += k
    return " ".join(out)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def finish(tid, task, parts, ws_done=True):
    d = os.path.join(OUT, tid)
    write(os.path.join(d, "task.json"), json.dumps(task, indent=2) + "\n")
    write(os.path.join(d, "check.json"), json.dumps(
        {"type": "parts", "deliverable": "report.md", "parts": parts}, indent=1) + "\n")
    write(os.path.join(d, "solution", "report.md"),
          "".join("%s: %s\n" % (p["id"], p["expect"]) for p in parts))


# ── documents: lookup through a registry (research-write) ──────────────────

TEAMS = ["atlas", "borealis", "cinder", "dynamo", "ember", "fjord", "garnet", "harbor", "iris", "juniper",
         "krypton", "lumen", "meridian", "nimbus", "onyx", "prism", "quartz", "relay", "summit", "tundra"]


def gen_docs(tid, n_docs, n_q, words):
    r = rng(tid)
    root = os.path.join(OUT, tid, "workspace")
    names = r.sample(TEAMS, n_docs) if n_docs <= len(TEAMS) else [TEAMS[i % 20] + str(i // 20) for i in range(n_docs)]
    svcs = []
    for i, team in enumerate(names):
        svcs.append({"id": "svc-%03d" % (i + 1), "team": team, "queue": "q-%s-%02d" % (r.choice("abcdefgh"), r.randint(10, 99)),
                     "page": r.choice([30, 45, 60, 90, 120, 180, 240, 300]) + r.randint(0, 9),
                     "keep": r.randint(7, 400), "replicas": r.randint(2, 40)})
    for i, s in enumerate(svcs):
        s["queue"] = "q-%s-%02d" % ("abcdefgh"[i % 8], 10 + i)  # unique
    for s in svcs:
        body = "# Service %s\n\nOwner team: %s\n\n%s\n\n## Operations\n\n- Consumes queue: %s\n- Paging threshold: %d seconds\n" \
               "- Retention: %d days\n- Replicas in production: %d\n\n%s\n" % (
                   s["id"], s["team"], prose(r, words // 2), s["queue"], s["page"], s["keep"], s["replicas"], prose(r, words // 2))
        write(os.path.join(root, "services", s["id"] + ".md"), body)
    reg = "# Queue registry\n\nWhich service consumes which queue.\n\n| Queue | Service |\n|---|---|\n" + "".join(
        "| %s | %s |\n" % (s["queue"], s["id"]) for s in r.sample(svcs, len(svcs)))
    write(os.path.join(root, "registry.md"), reg)
    chosen = r.sample(svcs, n_q)
    qs, parts = [], []
    kinds = ["page", "keep", "replicas"]
    label = {"page": "paging threshold in seconds", "keep": "retention in days", "replicas": "production replica count"}
    for i, s in enumerate(chosen):
        k = kinds[i % 3]
        qs.append("Q%d. What is the %s of the service that consumes queue %s?" % (i + 1, label[k], s["queue"]))
        parts.append({"id": "Q%d" % (i + 1), "kind": "answer", "key": "Q%d" % (i + 1), "match": "number",
                      "expect": s[k], "tol": 0, "desc": "registry hop then service doc"})
    write(os.path.join(root, "QUESTIONS.md"), "# Questions\n\n" + "\n".join(qs) + "\n")
    write(os.path.join(root, "README.md"), "Service documentation: services/ holds one document per service, "
          "registry.md maps queues to services, QUESTIONS.md has the questions.\n")
    finish(tid, {"family": "research-write", "title": "Service documentation lookups (%d docs)" % n_docs,
                 "prompt": "Answer the %d independent questions in QUESTIONS.md using the %d service documents in services/ and "
                           "registry.md (corpus only, no web). %s" % (n_q, n_docs, FORMAT),
                 "deliverable": "report.md"}, parts)


# ── modules: one seeded flaw per module (code-fix) ──────────────────────────

# name, signature, docstring, good body, flawed body
TPL = [
    ("count_inclusive", "lo, hi", "Count of integers from lo to hi, both ends included (0 if hi < lo).",
     "return max(0, hi - lo + 1)", "return max(0, hi - lo)"),
    ("clamp", "x, lo, hi", "Limit x to the closed interval [lo, hi].",
     "return min(max(x, lo), hi)", "return max(min(x, lo), hi)"),
    ("mean_present", "xs", "Mean of the values that are not None; 0.0 when there are none.",
     "v = [x for x in xs if x is not None]\n    return sum(v) / len(v) if v else 0.0",
     "v = [x for x in xs if x is not None]\n    return sum(v) / len(xs) if xs else 0.0"),
    ("is_leap", "y", "True for Gregorian leap years.",
     "return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)", "return y % 4 == 0 and y % 100 != 0"),
    ("median", "xs", "Median of a non-empty list; the mean of the two middle values for even lengths.",
     "s = sorted(xs)\n    m = len(s) // 2\n    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2",
     "s = sorted(xs)\n    m = len(s) // 2\n    return s[m]"),
    ("dedupe", "xs", "Remove duplicates, keeping the first occurrence and the original order.",
     "seen = set()\n    out = []\n    for x in xs:\n        if x not in seen:\n            seen.add(x)\n            out.append(x)\n    return out",
     "seen = {}\n    for i, x in enumerate(xs):\n        seen[x] = i\n    return [x for x, _ in sorted(seen.items(), key=lambda kv: kv[1])]"),
    ("percent", "part, whole", "part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0.",
     "return round(100.0 * part / whole, 1) if whole else 0.0", "return round(100.0 * part / whole, 1)"),
    ("chunk", "xs, n", "Split xs into consecutive lists of n items; the last one may be shorter.",
     "return [xs[i:i + n] for i in range(0, len(xs), n)]", "return [xs[i:i + n] for i in range(0, len(xs) - n + 1, n)]"),
    ("word_count", "s", "Number of whitespace-separated words (runs of spaces count once).",
     "return len(s.split())", "return len(s.split(' '))"),
    ("last_n", "xs, n", "The last n items of xs; an empty list when n <= 0.",
     "return xs[-n:] if n > 0 else []", "return xs[-n:]"),
    ("grade", "score", "'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'.",
     "return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'",
     "return 'A' if score > 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'"),
    ("tidy_name", "s", "Strip the text, collapse inner whitespace runs to one space, title-case it.",
     "return ' '.join(s.split()).title()", "return s.strip().title()"),
    ("days_apart", "a, b", "Absolute number of days between two day numbers.",
     "return abs(a - b)", "return b - a"),
    ("ceil_div", "a, b", "Ceiling of a / b for positive integers.",
     "return -(-a // b)", "return a // b + 1"),
    ("weighted_sum", "vals, weights", "Sum of value times weight; ValueError when the lengths differ.",
     "if len(vals) != len(weights):\n        raise ValueError('length mismatch')\n    return sum(v * w for v, w in zip(vals, weights))",
     "return sum(v * w for v, w in zip(vals, weights))"),
    ("starts_with_any", "s, prefixes", "True when s starts with at least one of the prefixes (False for no prefixes).",
     "return any(s.startswith(p) for p in prefixes)", "return all(s.startswith(p) for p in prefixes)"),
    ("running_total", "xs", "Cumulative sums, same length as xs.",
     "out, t = [], 0\n    for x in xs:\n        t += x\n        out.append(t)\n    return out",
     "out, t = [], 0\n    for x in xs:\n        out.append(t)\n        t += x\n    return out"),
]
DOMAINS = ["billing", "inventory", "shipping", "payroll", "search", "alerts", "reports", "quota", "audit", "sync",
           "catalog", "invoices"]


def gen_modules(tid, n_mod, n_fn):
    r = rng(tid)
    root = os.path.join(OUT, tid, "workspace")
    doms = r.sample(DOMAINS, n_mod)
    parts, qs = [], []
    for mi, dom in enumerate(doms):
        picks = r.sample(TPL, n_fn)
        bad = r.randrange(n_fn)
        lines = ['"""%s helpers."""\n' % dom.capitalize()]
        expect = None
        for i, (name, sig, doc, good, flaw) in enumerate(picks):
            fname = "%s_%s" % (name, dom)
            if i == bad:
                expect = fname
            body = flaw if i == bad else good
            lines.append("\ndef %s(%s):\n    \"\"\"%s\"\"\"\n    %s\n" % (fname, sig, doc, body))
        write(os.path.join(root, "%s.py" % dom), "".join(lines))
        qs.append("Q%d. Exactly one function in %s.py does not do what its docstring says. Which one? Answer with the function name." % (mi + 1, dom))
        parts.append({"id": "Q%d" % (mi + 1), "kind": "answer", "key": "Q%d" % (mi + 1), "match": "text",
                      "expect": expect, "desc": "seeded flaw in %s.py" % dom})
    write(os.path.join(root, "QUESTIONS.md"), "# Questions\n\n" + "\n".join(qs) + "\n")
    write(os.path.join(root, "README.md"), "%d small Python modules (stdlib only). Do not modify them: this is an audit.\n" % n_mod)
    finish(tid, {"family": "code-fix", "title": "Audit %d modules for a seeded flaw each" % n_mod,
                 "prompt": "The working directory holds %d independent Python modules (*.py) with docstring specs. Audit them: "
                           "in each module exactly one function contradicts its docstring. Answer the questions in QUESTIONS.md "
                           "(read the code and try the functions with python3). Do not edit the modules. %s" % (n_mod, FORMAT),
                 "deliverable": "report.md"}, parts)


# ── CSV files: one computation per file (data-audit) ────────────────────────

def gen_csv(tid, n_files, rows):
    r = rng(tid)
    root = os.path.join(OUT, tid, "workspace")
    regions = r.sample(["north", "south", "east", "west", "central", "coastal", "alpine", "delta", "island"], n_files)
    skus = ["SKU%02d" % i for i in range(1, 13)]
    parts, qs = [], []
    for fi, reg in enumerate(regions):
        data = []
        for oi in range(rows):
            data.append({"order_id": "%s-%05d" % (reg[:2].upper(), oi + 1), "day": r.randint(1, 90),
                         "sku": r.choice(skus), "qty": r.randint(1, 9), "unit_price_cents": r.randint(150, 9900),
                         "status": r.choice(["shipped"] * 7 + ["cancelled"] * 2 + ["returned"])})
        os.makedirs(root, exist_ok=True)
        with open(os.path.join(root, "%s.csv" % reg), "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(data[0]))
            w.writeheader()
            w.writerows(data)
        k = fi % 3
        sku = r.choice(skus)
        if k == 0:
            val = sum(d["qty"] * d["unit_price_cents"] for d in data if d["status"] == "shipped" and d["sku"] == sku) / 100.0
            q, tol = "total revenue in euros (qty times unit price, divided by 100) of rows with status shipped for %s, 2 decimals" % sku, 0.01
        elif k == 1:
            val = sum(1 for d in data if d["status"] == "cancelled" and d["day"] > 60)
            q, tol = "number of cancelled orders placed after day 60 (day > 60)", 0
        else:
            val = max(d["qty"] * d["unit_price_cents"] for d in data if d["status"] != "cancelled") / 100.0
            q, tol = "the largest single order value in euros (qty times unit price, divided by 100) among rows that are not cancelled, 2 decimals", 0.01
        qs.append("Q%d. In %s.csv: %s." % (fi + 1, reg, q))
        parts.append({"id": "Q%d" % (fi + 1), "kind": "answer", "key": "Q%d" % (fi + 1), "match": "number",
                      "expect": round(val, 2), "tol": tol, "desc": "one computation on %s.csv" % reg})
    write(os.path.join(root, "QUESTIONS.md"), "# Questions\n\n" + "\n".join(qs) + "\n")
    write(os.path.join(root, "README.md"), "One CSV per sales region (order_id, day 1-90, sku, qty, unit_price_cents, status). "
          "Each question concerns one file only.\n")
    finish(tid, {"family": "data-audit", "title": "Per-region order files (%d files)" % n_files,
                 "prompt": "The working directory holds %d CSV files, one per region, README.md and QUESTIONS.md (%d independent "
                           "questions). Compute with the query_data tool or python3 scripts. %s" % (n_files, n_files, FORMAT),
                 "deliverable": "report.md"}, parts)


def main():
    shutil.rmtree(OUT, ignore_errors=True)
    gen_docs("p01", 40, 8, 520)
    gen_docs("p02", 56, 9, 600)
    gen_modules("p03", 8, 8)
    gen_modules("p04", 6, 12)
    gen_csv("p05", 8, 2500)
    gen_csv("p06", 6, 4000)


if __name__ == "__main__":
    main()
