#!/usr/bin/env python3
"""Generate the data-audit family (ld01..ld06) of the EV-7 long task set (AGE-826).

    python3 evals/swarm-long/gen_data.py

Each task is a small business database (4-6 CSV files at the workspace root),
a README.md data dictionary with the business definitions, and QUESTIONS.md
with 8 questions. The expected answers are computed here, in plain Python,
from the CSV files exactly as written (they are read back), never from SQL.
`solution/report.md` is the reference deliverable built from those values.

Every task has a fixed seed: a re-run reproduces the bytes exactly. The
script deletes and rewrites only tasks/ld01..ld06.

Run with `--traps` to print, per task, which answers change when one trap of
the README is ignored (a sanity check that every trap bites).

Python 3.6+, stdlib only.
"""

import csv
import datetime
import json
import math
import os
import random
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TASKS = os.path.join(HERE, "tasks")

D = datetime.date
DT = datetime.datetime
TD = datetime.timedelta

SHOW_TRAPS = "--traps" in sys.argv


# ---------------------------------------------------------------- helpers

def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def piso(s):
    return DT.strptime(s, "%Y-%m-%dT%H:%M:%SZ")


def pdate(s):
    return DT.strptime(s, "%Y-%m-%d").date()


def plocal(s):
    return DT.strptime(s, "%Y-%m-%d %H:%M")


def ym(d):
    return "%04d-%02d" % (d.year, d.month)


def r2(x):
    return round(x + (1e-9 if x >= 0 else -1e-9), 2)


def cents(s):
    """'12.34' -> 1234 (exact)."""
    neg = s.startswith("-")
    s = s.lstrip("-")
    if "." in s:
        whole, frac = s.split(".")
        frac = (frac + "00")[:2]
    else:
        whole, frac = s, "00"
    v = int(whole) * 100 + int(frac)
    return -v if neg else v


def money(c):
    return "%d.%02d" % (c // 100, c % 100)


def median(xs):
    xs = sorted(xs)
    n = len(xs)
    if n % 2:
        return xs[n // 2]
    return (xs[n // 2 - 1] + xs[n // 2]) / 2.0


def write_csv(path, header, rows):
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(header)
        for row in rows:
            w.writerow(row)


def read_csv(path):
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def top(scores, margin, tie_key=None):
    """The best key of `scores`; asserts it beats the runner-up by `margin`."""
    ranked = sorted(scores.items(), key=lambda kv: (-kv[1], kv[0]))
    if len(ranked) > 1:
        assert ranked[0][1] - ranked[1][1] >= margin, ("too close", ranked[:3])
    return ranked[0][0]


def bottom(scores, margin):
    ranked = sorted(scores.items(), key=lambda kv: (kv[1], kv[0]))
    if len(ranked) > 1:
        assert ranked[1][1] - ranked[0][1] >= margin, ("too close", ranked[:3])
    return ranked[0][0]


def fmt_value(q, v):
    if q["match"] == "number":
        return "%.*f" % (q.get("dec", 0), v)
    if q["match"] == "set":
        return ", ".join(sorted(v))
    return str(v)


PROMPT = (
    "You are auditing {domain} data. Your working directory holds the data as CSV files "
    "at its root ({files}), README.md (the data dictionary and the business definitions; "
    "follow them exactly, they override any intuition) and QUESTIONS.md ({n} questions, "
    "Q1..Q{n}). The files are too large to read whole: compute with the query_data tool "
    "or with python3 scripts, and check your joins and filters against the README rules.\n\n"
    "Answer every question in QUESTIONS.md. Write report.md in the working directory. It "
    "must START with one line per question, in order, exactly `Qn: <answer>` (for example "
    "`Q1: 1234`), each answer in the format the question states, followed by your working "
    "notes (do not start any other line with `Qn:`; the last such line is the one "
    "that counts). End your final chat answer with the same Q1..Q{n} lines."
)


def emit(tid, title, domain, files, readme, questions, intro):
    """Write tasks/<tid>/ from the generated rows and the answered questions."""
    d = os.path.join(TASKS, tid)
    if os.path.isdir(d):
        shutil.rmtree(d)
    ws = os.path.join(d, "workspace")
    os.makedirs(ws)
    os.makedirs(os.path.join(d, "solution"))
    for name, (header, rows) in files.items():
        write_csv(os.path.join(ws, name), header, rows)
    with open(os.path.join(ws, "README.md"), "w") as f:
        f.write(readme.strip() + "\n")
    lines = ["# Questions", "", intro.strip(), "",
             "Write each answer on its own line `Qn: <answer>` at the top of report.md, "
             "in the format stated with the question.", ""]
    for i, q in enumerate(questions, 1):
        q["key"] = "Q%d" % i
        lines.append("**Q%d.** %s" % (i, q["text"].strip()))
        lines.append("")
        lines.append("Format: %s" % q["fmt"])
        lines.append("")
    with open(os.path.join(ws, "QUESTIONS.md"), "w") as f:
        f.write("\n".join(lines).rstrip() + "\n")
    names = sorted(files)
    prompt = PROMPT.format(domain=domain, files=", ".join(names), n=len(questions))
    with open(os.path.join(d, "task.json"), "w") as f:
        json.dump({"family": "data-audit", "title": title, "prompt": prompt,
                   "deliverable": "report.md"}, f, indent=2)
        f.write("\n")
    parts = []
    for q in questions:
        p = {"id": q["key"], "kind": "answer", "key": q["key"], "match": q["match"],
             "desc": q["desc"]}
        if q["match"] == "number":
            p["expect"] = q["expect"]
            p["tol"] = q["tol"]
        elif q["match"] == "set":
            p["expect"] = sorted(q["expect"])
        else:
            p["expect"] = q["expect"]
            if q.get("accept"):
                p["accept"] = q["accept"]
        parts.append(p)
    with open(os.path.join(d, "check.json"), "w") as f:
        json.dump({"type": "parts", "deliverable": "report.md", "parts": parts}, f, indent=2)
        f.write("\n")
    rep = ["%s: %s" % (q["key"], fmt_value(q, q["expect"])) for q in questions]
    rep += ["", "## Working notes", "",
            "Reference answers computed by evals/swarm-long/gen_data.py from the CSV files "
            "with the README rules applied.", ""]
    for q in questions:
        rep.append("- question %s - %s" % (q["key"][1:], q["desc"]))
    with open(os.path.join(d, "solution", "report.md"), "w") as f:
        f.write("\n".join(rep) + "\n")


def trap_report(tid, compute, data, traps):
    """Print which answers change when a trap is ignored."""
    if not SHOW_TRAPS:
        return
    base = compute(data, set())
    print("%s traps:" % tid)
    for t in traps:
        alt = compute(data, {t})
        changed = [k for k in base if not same(base[k], alt[k])]
        print("  %-14s changes %s" % (t, ", ".join(changed) or "NOTHING"))


def same(a, b):
    if isinstance(a, float) or isinstance(b, float):
        try:
            return abs(float(a) - float(b)) < 0.02
        except (TypeError, ValueError):
            return False
    return a == b


def load(tid, names):
    ws = os.path.join(TASKS, tid, "workspace")
    return {n: read_csv(os.path.join(ws, n)) for n in names}


def latest(rows, key, stamp, parse=piso):
    """Keep, per `key`, the row with the latest `stamp`."""
    best = {}
    for r in rows:
        k = r[key]
        if k not in best or parse(r[stamp]) > parse(best[k][stamp]):
            best[k] = r
    return best


def first_kept(rows, key):
    best = {}
    for r in rows:
        best.setdefault(r[key], r)
    return best


# ===================================================================== ld01
# Retail orders across eight stores in five currencies.

LD01_STORES = [
    ("S01", "Amsterdam", "EMEA", "EUR", 1),
    ("S02", "Berlin", "EMEA", "EUR", 1),
    ("S03", "London", "EMEA", "GBP", 0),
    ("S04", "New York", "AMER", "USD", -5),
    ("S05", "Chicago", "AMER", "USD", -6),
    ("S06", "Tokyo", "APAC", "JPY", 9),
    ("S07", "Sydney", "APAC", "AUD", 11),
    ("S08", "Toronto", "AMER", "CAD", -5),
]
LD01_MONTHS = ["2025-12", "2026-01", "2026-02", "2026-03", "2026-04"]
LD01_FX = {  # units of currency per 1 EUR
    "EUR": [1.0, 1.0, 1.0, 1.0, 1.0],
    "GBP": [0.8462, 0.8517, 0.8381, 0.8424, 0.8473],
    "USD": [1.0412, 1.0873, 1.0619, 1.0934, 1.0781],
    "JPY": [162.41, 158.93, 161.72, 157.18, 159.84],
    "AUD": [1.6523, 1.6714, 1.6482, 1.6891, 1.6604],
    "CAD": [1.4781, 1.4923, 1.5012, 1.4869, 1.4951],
}
# orders per local month (Jan, Feb, Mar) per store
LD01_VOLUME = {
    "S01": (150, 140, 205), "S02": (160, 165, 150), "S03": (140, 130, 190),
    "S04": (170, 160, 150), "S05": (130, 125, 185), "S06": (150, 160, 130),
    "S07": (120, 110, 175), "S08": (150, 155, 145),
}


def ld01_generate():
    rng = random.Random(82601)
    customers = []
    for i in range(1, 481):
        cid = "C%04d" % i
        signup = D(2024, 1, 1) + TD(days=rng.randint(0, 720))
        customers.append([cid, signup.isoformat(), 0])
    tests = rng.sample(range(480), 14)
    for t in tests:
        customers[t][2] = 1
    heavy_test = customers[tests[0]][0]
    weights = [1.0 / (1 + i) ** 0.55 for i in range(480)]
    order = list(range(480))
    rng.shuffle(order)
    cust_w = [0] * 480
    for rank, idx in enumerate(order):
        cust_w[idx] = weights[rank]
    cust_w[tests[0]] = weights[0] * 1.6  # a busy test account

    store_cur = {s[0]: s[3] for s in LD01_STORES}
    store_off = {s[0]: s[4] for s in LD01_STORES}

    def fx(cur, month):
        return LD01_FX[cur][LD01_MONTHS.index(month)]

    orders = []  # (order_id, cust, store, utc, channel, gross_str, status)
    oid = 100001
    plan = []
    for s in LD01_STORES:
        sid = s[0]
        jan, feb, mar = LD01_VOLUME[sid]
        for (y, m, n, ndays) in ((2026, 1, jan, 31), (2026, 2, feb, 28), (2026, 3, mar, 31)):
            for _ in range(n):
                plan.append((sid, DT(y, m, 1) + TD(seconds=rng.randint(0, ndays * 86400 - 1))))
        # boundary orders just outside Q1 (local)
        for _ in range(14):
            plan.append((sid, DT(2025, 12, 31) + TD(seconds=rng.randint(0, 86399))))
            plan.append((sid, DT(2026, 4, 1) + TD(seconds=rng.randint(0, 86399))))
    rng.shuffle(plan)
    plan.sort(key=lambda p: p[1] - TD(hours=store_off[p[0]]))
    for sid, local in plan:
        utc = local - TD(hours=store_off[sid])
        cur = store_cur[sid]
        ci = rng.choices(range(480), weights=cust_w)[0]
        base = rng.lognormvariate(4.3, 0.62)
        amount = base * fx(cur, ym(local))
        if cur == "JPY":
            gross = "%d" % int(round(amount))
        else:
            gross = "%.2f" % amount
        channel = rng.choices(["web", "store", "app"], weights=[45, 35, 20])[0]
        status = rng.choices(["completed", "cancelled", "pending"], weights=[86, 9, 5])[0]
        orders.append(["O%d" % oid, customers[ci][0], sid, utc, channel, gross, status])
        oid += 1

    rows = []
    final = {}
    variants = ["completed", "Completed", "COMPLETED", "completed ", " Completed"]
    for o in orders:
        oid_, cid, sid, utc, channel, gross, status = o
        ing = utc + TD(minutes=rng.randint(2, 90))

        def show(st):
            if st == "completed" and rng.random() < 0.18:
                return rng.choice(variants[1:])
            return st
        r = rng.random()
        if r < 0.11:
            # re-sent: first version, then a change
            kind = rng.random()
            if kind < 0.35:
                first, last = ("completed", gross), ("cancelled", gross)
            elif kind < 0.6:
                first, last = ("pending", gross), ("completed", gross)
            elif kind < 0.85:
                if store_cur[sid] == "JPY":
                    new = "%d" % int(int(gross) * rng.uniform(0.5, 0.9))
                else:
                    new = "%.2f" % (float(gross) * rng.uniform(0.5, 0.9))
                first, last = ("completed", gross), ("completed", new)
            else:
                first, last = (status, gross), (status, gross)
            ing2 = ing + TD(hours=rng.randint(3, 120), minutes=rng.randint(0, 59))
            rows.append([oid_, cid, sid, iso(utc), channel, first[1], show(first[0]), iso(ing)])
            rows.append([oid_, cid, sid, iso(utc), channel, last[1], show(last[0]), iso(ing2)])
            final[oid_] = (last[0], last[1], sid, utc)
        else:
            rows.append([oid_, cid, sid, iso(utc), channel, gross, show(status), iso(ing)])
            final[oid_] = (status, gross, sid, utc)
    # file order: by order timestamp, re-sent pairs in random order
    rng.shuffle(rows)
    rows.sort(key=lambda r: r[3])

    refunds = []
    rid = 1
    for oid_ in sorted(final):
        status, gross, sid, utc = final[oid_]
        c = cents(gross) if store_cur[sid] != "JPY" else int(gross) * 100
        p = 0.14 if status == "completed" else (0.3 if status == "cancelled" else 0.0)
        if rng.random() >= p:
            continue
        kind = rng.random()
        if kind < 0.3:
            parts = [c]
        elif kind < 0.42:
            a = int(c * rng.uniform(0.3, 0.7))
            parts = [a, c - a]
        elif kind < 0.8:
            parts = [int(c * rng.uniform(0.1, 0.6))]
        else:
            a = int(c * rng.uniform(0.1, 0.3))
            parts = [a, int(c * rng.uniform(0.1, 0.3))]
        t = utc
        for a in parts:
            t = t + TD(days=rng.randint(1, 30), hours=rng.randint(0, 23))
            if store_cur[sid] == "JPY":
                amt = "%d" % (a // 100)
            else:
                amt = money(a)
            refunds.append(["R%05d" % rid, oid_, iso(t), amt])
            rid += 1
    refunds.sort(key=lambda r: r[2])

    files = {
        "orders.csv": (["order_id", "customer_id", "store_id", "order_ts_utc", "channel",
                        "gross_amount", "status", "ingested_at"], rows),
        "refunds.csv": (["refund_id", "order_id", "refunded_at_utc", "amount"], refunds),
        "stores.csv": (["store_id", "city", "region", "currency", "utc_offset_hours"],
                       [list(s) for s in LD01_STORES]),
        "customers.csv": (["customer_id", "signup_date", "is_test"], customers),
        "fx_rates.csv": (["month", "currency", "units_per_eur"],
                         [[m, cur, LD01_FX[cur][i]] for i, m in enumerate(LD01_MONTHS)
                          for cur in sorted(LD01_FX)]),
    }
    return files, heavy_test


def ld01_compute(data, naive):
    stores = {s["store_id"]: s for s in data["stores.csv"]}
    test = {c["customer_id"] for c in data["customers.csv"] if c["is_test"] == "1"}
    fx = {(r["month"], r["currency"]): float(r["units_per_eur"]) for r in data["fx_rates.csv"]}
    if "dedup" in naive:
        cur = first_kept(data["orders.csv"], "order_id")
    else:
        cur = latest(data["orders.csv"], "order_id", "ingested_at")
    valid = {}
    for oid, r in cur.items():
        st = r["status"] if "status_case" in naive else r["status"].strip().lower()
        if st != "completed":
            continue
        if "test" not in naive and r["customer_id"] in test:
            continue
        s = stores[r["store_id"]]
        utc = piso(r["order_ts_utc"])
        local = utc if "tz" in naive else utc + TD(hours=int(s["utc_offset_hours"]))
        month = ym(local)
        rate = fx[(month, s["currency"])]
        if "fx_mult" in naive:
            g = float(r["gross_amount"]) * rate
        else:
            g = float(r["gross_amount"]) / rate
        valid[oid] = {"row": r, "date": local.date(), "month": month, "rate": rate,
                      "gross_local": float(r["gross_amount"]), "gross": g, "refund_local": 0.0,
                      "refund": 0.0, "store": r["store_id"], "region": s["region"],
                      "cust": r["customer_id"], "channel": r["channel"]}
    for f in data["refunds.csv"]:
        v = valid.get(f["order_id"])
        if v is None:
            continue
        if "refund_month" in naive:
            continue
        a = float(f["amount"])
        v["refund_local"] += a
        v["refund"] += a * v["gross"] / v["gross_local"] if v["gross_local"] else 0.0
    q1 = [v for v in valid.values() if "2026-01" <= v["month"] <= "2026-03"]
    out = {}
    out["Q1"] = sum(1 for v in valid.values() if v["month"] == "2026-02")
    out["Q2"] = r2(sum(v["gross"] - v["refund"] for v in q1))
    reg = {}
    for v in q1:
        if v["month"] == "2026-03":
            reg[v["region"]] = reg.get(v["region"], 0) + v["gross"] - v["refund"]
    out["Q3"] = sorted(reg.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    app = [v for v in q1 if v["channel"] == "app"]
    out["Q4"] = r2(100.0 * sum(v["refund"] for v in app) / sum(v["gross"] for v in app))
    cust = {}
    for v in q1:
        cust[v["cust"]] = cust.get(v["cust"], 0) + v["gross"] - v["refund"]
    out["Q5"] = sorted(cust.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    out["Q6"] = sum(1 for v in q1 if v["refund_local"] >= v["gross_local"] - 1e-9)
    out["Q7"] = r2(median([v["gross"] for v in q1 if v["store"] == "S06"]))
    feb, mar = {}, {}
    for v in q1:
        bucket = feb if v["month"] == "2026-02" else mar if v["month"] == "2026-03" else None
        if bucket is not None:
            bucket[v["store"]] = bucket.get(v["store"], 0) + v["gross"] - v["refund"]
    out["Q8"] = sorted(s for s in feb if mar.get(s, 0) > 1.10 * feb[s])
    out["_ratios"] = {s: mar.get(s, 0) / feb[s] for s in feb}
    out["_reg"] = reg
    out["_cust"] = cust
    return out


LD01_README = """
# Retail orders: data dictionary

Order data of eight stores for the first quarter of 2026, exported from the
order feed. All files are comma-separated with a header row.

## Files

### orders.csv
One row per order **version** (the feed re-sends an order when it changes).

| column | meaning |
| -- | -- |
| order_id | order identifier |
| customer_id | customer, see customers.csv |
| store_id | store, see stores.csv |
| order_ts_utc | when the order was placed, UTC (ISO 8601, `Z`) |
| channel | `web`, `store` or `app` |
| gross_amount | order value in the **store's** currency (see stores.csv); JPY has no decimals |
| status | `completed`, `cancelled` or `pending` (see rule 2) |
| ingested_at | when this version reached the warehouse, UTC |

### refunds.csv
Money returned to customers. `amount` is in the currency of the order's store.
An order can have several refunds.

### stores.csv
`store_id`, `city`, `region`, `currency` and `utc_offset_hours`: the store's
fixed offset from UTC for the whole period (local time = UTC + offset; no
daylight-saving changes are applied).

### customers.csv
`customer_id`, `signup_date`, `is_test` (1 = internal test account).

### fx_rates.csv
Monthly rates: `units_per_eur` = how many units of `currency` buy one euro in
that `month` (YYYY-MM). EUR rows are 1.

## Business definitions

1. **Current version.** When an `order_id` appears more than once, only the
   row with the latest `ingested_at` counts; earlier rows are superseded,
   whatever order they appear in.
2. **Valid order.** The current version has status `completed`. Status values
   are case-insensitive and may carry stray spaces. Cancelled and pending
   orders are not revenue. Orders of test customers (`is_test` = 1) are
   excluded from every figure.
3. **Order date.** The store-local calendar date of `order_ts_utc` (UTC plus
   the store's `utc_offset_hours`). Every month, quarter or date filter uses
   the order date. Q1 2026 = order dates 2026-01-01 to 2026-03-31.
4. **Euro conversion.** EUR = local amount / `units_per_eur` of the store's
   currency for the month of the order date.
5. **Refunds** belong to their order: they count in the order's month (not
   the month the refund was paid) and convert at the order's rate. Refunds of
   orders that are not valid are ignored.
6. **Net revenue** = gross amount minus refunds, of valid orders, in EUR.
7. **Fully refunded** = the order's refunds add up to at least its gross
   amount.

Round only final answers, not intermediate values.
"""


def ld01():
    files, heavy_test = ld01_generate()
    emit_stub("ld01", files)
    data = load("ld01", list(files))
    a = ld01_compute(data, set())
    for s, r in a["_ratios"].items():
        assert abs(r - 1.10) > 0.04, ("ratio too close to 10%", s, r)
    top(a["_reg"], 500)
    top(a["_cust"], 30)
    qs = [
        dict(text="How many valid orders have an order date in February 2026?",
             fmt="an integer, e.g. `Q1: 1234`.", match="number", expect=a["Q1"], tol=0,
             desc="count Feb orders: needs dedup by latest ingested_at, status case, test customers, store-local date"),
        dict(text="What is the total net revenue in EUR of valid orders dated in Q1 2026?",
             fmt="EUR, 2 decimals, no currency sign, e.g. `Q2: 123456.78`.", match="number",
             expect=a["Q2"], tol=1.0, dec=2,
             desc="Q1 net EUR: FX divide by units_per_eur per order month, refunds attributed to order, local dates"),
        dict(text="Which region had the highest net revenue in EUR in March 2026?",
             fmt="the region name exactly as in stores.csv.", match="text", expect=a["Q3"],
             desc="March net by region: FX trap (JPY/GBP volumes), refunds netted"),
        dict(text="What was the refund rate of the `app` channel in Q1 2026, i.e. refunds in EUR "
                  "divided by gross revenue in EUR of valid app orders, as a percentage?",
             fmt="a percentage with 2 decimals, no % sign needed, e.g. `Q4: 7.25`.", match="number",
             expect=a["Q4"], tol=0.02, dec=2,
             desc="app refund rate: refunds of cancelled orders ignored, refunds after March still count"),
        dict(text="Which customer had the highest net revenue in EUR in Q1 2026? Ties go to the "
                  "lowest customer_id.",
             fmt="the customer_id, e.g. `Q5: C0001`.", match="text", expect=a["Q5"],
             desc="top customer: a heavy test account (%s) must be excluded" % heavy_test),
        dict(text="How many valid orders dated in Q1 2026 were fully refunded?",
             fmt="an integer.", match="number", expect=a["Q6"], tol=0,
             desc="fully refunded: sums multiple partial refunds per order vs current gross"),
        dict(text="What is the median gross amount in EUR of the valid orders of store S06 dated "
                  "in Q1 2026? With an even number of orders, take the mean of the two middle values.",
             fmt="EUR with 2 decimals, e.g. `Q7: 64.10`.", match="number", expect=a["Q7"],
             tol=0.011, dec=2,
             desc="S06 median: JPY conversion per month and UTC+9 date boundary"),
        dict(text="Which stores had a March 2026 net revenue in EUR more than 10% above their "
                  "February 2026 net revenue?",
             fmt="the store_ids, comma-separated, e.g. `Q8: S01, S02`.", match="set",
             expect=a["Q8"],
             desc="store growth set: per-store net EUR Feb vs Mar, local month boundaries"),
    ]
    emit("ld01", "Retail orders Q1 2026 audit", "retail order", files, LD01_README, qs,
         "All questions use the definitions in README.md.")
    trap_report("ld01", ld01_compute, data,
                ["dedup", "status_case", "test", "tz", "fx_mult", "refund_month"])


def emit_stub(tid, files):
    """Write only the CSV files, so the answers can be computed from them."""
    ws = os.path.join(TASKS, tid, "workspace")
    if os.path.isdir(os.path.join(TASKS, tid)):
        shutil.rmtree(os.path.join(TASKS, tid))
    os.makedirs(ws)
    for name, (header, rows) in files.items():
        write_csv(os.path.join(ws, name), header, rows)


# ===================================================================== ld02
# SaaS subscriptions: MRR from invoices, annual plans, credit notes, FX.

LD02_MONTHS = ["%04d-%02d" % (2024 + (6 + i) // 12, (6 + i) % 12 + 1) for i in range(24)]  # 2024-07..2026-06
LD02_PLANS = [
    ("STARTER-M", "starter", "monthly", 49), ("STARTER-A", "starter", "annual", 490),
    ("TEAM-M", "team", "monthly", 199), ("TEAM-A", "team", "annual", 1990),
    ("BUSINESS-M", "business", "monthly", 799), ("BUSINESS-A", "business", "annual", 7990),
]


def ld02_fx(rng):
    fx = {}
    e, g = 1.09, 1.27
    for m in LD02_MONTHS:
        e += rng.uniform(-0.015, 0.015)
        g += rng.uniform(-0.02, 0.02)
        fx[(m, "USD")] = 1.0
        fx[(m, "EUR")] = round(e, 4)
        fx[(m, "GBP")] = round(g, 4)
    return fx


def madd(m, k):
    i = LD02_MONTHS.index(m) + k if m in LD02_MONTHS else None
    y, mo = int(m[:4]), int(m[5:])
    t = y * 12 + mo - 1 + k
    return "%04d-%02d" % (t // 12, t % 12 + 1)


def ld02_generate():
    rng = random.Random(82602)
    fx = ld02_fx(rng)
    price = {p[0]: p[3] for p in LD02_PLANS}
    accounts, invoices, credits = [], [], []
    inv_no = [50001]
    seg_tiers = {"smb": (["starter", "team"], [70, 30]),
                 "mid_market": (["team", "business"], [65, 35]),
                 "enterprise": (["team", "business"], [25, 75])}
    seg_qty = {"smb": (1, 3), "mid_market": (2, 8), "enterprise": (6, 25)}
    internal = set(rng.sample(range(1, 431), 9))

    def new_invoice(acc, plan, start, cur, amount_usd, when):
        local = amount_usd / fx[(start, cur)]
        inv = ["INV%06d" % inv_no[0], acc, plan, start + "-01", "%.2f" % local, cur]
        inv_no[0] += 1
        return inv

    for i in range(1, 431):
        acc = "A%04d" % i
        seg = rng.choices(["smb", "mid_market", "enterprise"], weights=[55, 30, 15])[0]
        if i in internal:
            seg = rng.choice(["mid_market", "enterprise"])
        cur = rng.choices(["USD", "EUR", "GBP"], weights=[55, 30, 15])[0]
        annual = rng.random() < 0.28
        tiers, w = seg_tiers[seg]
        tier = rng.choices(tiers, weights=w)[0]
        qlo, qhi = seg_qty[seg]
        qty = rng.randint(qlo, qhi)
        disc = rng.choice([1.0, 1.0, 0.9, 0.85, 0.8])
        start_idx = rng.randint(0, 23)
        if rng.random() < 0.3:
            start_idx = rng.randint(0, 5)
        elif rng.random() < 0.1:
            start_idx = rng.randint(21, 23)
        start = LD02_MONTHS[start_idx]
        created = DT(int(start[:4]), int(start[5:]), 1) - TD(days=rng.randint(0, 20))
        country = {"USD": rng.choice(["US", "US", "CA"]), "EUR": rng.choice(["DE", "NL", "FR"]),
                   "GBP": "GB"}[cur]
        accounts.append([acc, "Company %s" % acc[1:], seg, country, cur,
                         1 if i in internal else 0, created.date().isoformat()])
        # active spans of months
        m = start
        events = []  # (period_start, plan_code)
        while m <= "2026-06":
            if annual:
                code = tier.upper() + "-A"
                events.append((m, code))
                if rng.random() < 0.2:
                    break
                m = madd(m, 12)
            else:
                code = tier.upper() + "-M"
                events.append((m, code))
                r = rng.random()
                if r < 0.035:
                    if rng.random() < 0.3:
                        m = madd(m, rng.randint(2, 4))  # pause, then reactivation
                        continue
                    break
                if r < 0.07:
                    up = {"starter": "team", "team": "business", "business": "business"}
                    tier = up[tier]
                elif r < 0.09:
                    down = {"starter": "starter", "team": "starter", "business": "team"}
                    tier = down[tier]
                m = madd(m, 1)
        for (ps, code) in events:
            if ps > "2026-06":
                continue
            amount_usd = price[code] * qty * disc * rng.uniform(0.97, 1.03)
            inv = new_invoice(acc, code, ps, cur, amount_usd, None)
            issued = DT(int(ps[:4]), int(ps[5:]), 1, rng.randint(0, 23), rng.randint(0, 59))
            r = rng.random()
            if r < 0.04:
                status = "open"
            elif r < 0.07:
                status = "void"
            else:
                status = "paid"
            if rng.random() < 0.1:
                first = rng.choice(["draft", "open"])
                last = rng.choice(["paid", "paid", "void"]) if status != "open" else "open"
                invoices.append(inv + [first, iso(issued)])
                invoices.append(inv + [last, iso(issued + TD(days=rng.randint(1, 25),
                                                              minutes=rng.randint(1, 600)))])
                status = last
            else:
                invoices.append(inv + [status, iso(issued)])
            if status == "void" and rng.random() < 0.6:
                re_usd = amount_usd * rng.uniform(0.85, 1.0)
                inv2 = new_invoice(acc, code, ps, cur, re_usd, None)
                invoices.append(inv2 + ["paid", iso(issued + TD(days=rng.randint(1, 5)))])
            amt_c = cents(inv[4])
            p = rng.random()
            if status in ("paid", "void") and p < 0.07:
                if p < 0.012:
                    parts = [amt_c]
                else:
                    parts = [int(amt_c * rng.uniform(0.1, 0.5))]
                for a in parts:
                    credits.append([None, inv[0], money(a),
                                    iso(issued + TD(days=rng.randint(3, 60))),
                                    rng.choice(["service credit", "billing error", "goodwill"])])
    rng.shuffle(invoices)
    invoices.sort(key=lambda r: (r[3], r[1]))
    credits.sort(key=lambda r: r[3])
    for k, c in enumerate(credits, 1):
        c[0] = "CN%05d" % k
    files = {
        "accounts.csv": (["account_id", "name", "segment", "country", "currency", "is_internal",
                          "created_at"], accounts),
        "plans.csv": (["plan_code", "tier", "billing_period", "list_price_usd"],
                      [list(p) for p in LD02_PLANS]),
        "invoices.csv": (["invoice_id", "account_id", "plan_code", "period_start", "amount",
                          "currency", "status", "ingested_at"], invoices),
        "credit_notes.csv": (["credit_note_id", "invoice_id", "amount", "issued_at", "reason"],
                             credits),
        "fx_rates.csv": (["month", "currency", "usd_per_unit"],
                         [[m, c, fx[(m, c)]] for m in LD02_MONTHS for c in ("EUR", "GBP", "USD")]),
    }
    return files


def ld02_compute(data, naive):
    plans = {p["plan_code"]: p for p in data["plans.csv"]}
    accs = {a["account_id"]: a for a in data["accounts.csv"]}
    fx = {(r["month"], r["currency"]): float(r["usd_per_unit"]) for r in data["fx_rates.csv"]}
    if "dedup" in naive:
        cur = first_kept(data["invoices.csv"], "invoice_id")
    else:
        cur = latest(data["invoices.csv"], "invoice_id", "ingested_at")
    credit = {}
    if "credits" not in naive:
        for c in data["credit_notes.csv"]:
            credit[c["invoice_id"]] = credit.get(c["invoice_id"], 0) + cents(c["amount"])
    mrr = {}   # (acc, month) -> usd
    netc = {}  # (acc, month) -> local cents (sign test)
    annual_mrr = {}
    tier_of = {}
    for inv in cur.values():
        ok = ("paid", "open") if "void" not in naive else ("paid", "open", "void")
        if inv["status"] not in ok:
            continue
        if "internal" not in naive and accs[inv["account_id"]]["is_internal"] == "1":
            continue
        plan = plans[inv["plan_code"]]
        ps = inv["period_start"][:7]
        net = cents(inv["amount"]) - credit.get(inv["invoice_id"], 0)
        rate = fx[(ps, inv["currency"])] if "fx" not in naive else 1.0
        usd = net / 100.0 * rate
        if plan["billing_period"] == "annual" and "annual" not in naive:
            months = [madd(ps, k) for k in range(12)]
            usd /= 12.0
        else:
            months = [ps]
        for m in months:
            key = (inv["account_id"], m)
            mrr[key] = mrr.get(key, 0) + usd
            netc[key] = netc.get(key, 0) + net
            if plan["billing_period"] == "annual":
                annual_mrr[key] = annual_mrr.get(key, 0) + usd
            tier_of.setdefault(key, []).append((usd, plan["tier"]))

    def total(m):
        return sum(v for (a, mm), v in mrr.items() if mm == m)

    def active(m):
        return {a for (a, mm), c in netc.items() if mm == m and c > 0}

    out = {}
    out["Q1"] = r2(total("2026-06"))
    out["Q2"] = len(active("2026-03"))
    out["Q3"] = len(active("2026-03") - active("2026-04"))
    jan = active("2026-01")
    out["Q4"] = r2(100.0 * sum(mrr.get((a, "2026-06"), 0) for a in jan)
                   / sum(mrr[(a, "2026-01")] for a in jan))
    grow = {}
    for (a, m), v in mrr.items():
        seg = accs[a]["country"]
        if m == "2026-06":
            grow[seg] = grow.get(seg, 0) + v
        elif m == "2026-01":
            grow[seg] = grow.get(seg, 0) - v
    out["Q5"] = sorted(grow.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    ent = [a for a in active("2026-04") if accs[a]["segment"] == "enterprise"]
    out["Q6"] = r2(sum(mrr[(a, "2026-04")] for a in ent) / len(ent))
    first = {}
    for (a, m), c in netc.items():
        if c > 0 and (a not in first or m < first[a]):
            first[a] = m
    if "reactivation" in naive:
        out["Q7"] = sum(len(active(m) - active(madd(m, -1))) for m in ("2026-04", "2026-05", "2026-06"))
    else:
        out["Q7"] = sum(1 for a, m in first.items() if "2026-04" <= m <= "2026-06")
    out["Q8"] = r2(100.0 * sum(v for (a, m), v in annual_mrr.items() if m == "2026-06")
                   / total("2026-06"))
    out["_grow"] = grow
    return out


LD02_README = """
# SaaS billing: data dictionary

Billing data of a B2B SaaS product, January 2025 export extended to June
2026 (some annual invoices start in 2024). Comma-separated, header row.

## Files

### invoices.csv
One row per invoice **version**: the billing system re-sends an invoice when
its status changes.

| column | meaning |
| -- | -- |
| invoice_id | invoice identifier |
| account_id | customer account, see accounts.csv |
| plan_code | plan, see plans.csv |
| period_start | first day of the service period the invoice pays for |
| amount | invoice total in `currency`, before credit notes |
| currency | USD, EUR or GBP |
| status | `draft`, `open` (issued, not yet paid), `paid` or `void` (cancelled; a corrected invoice may be issued under a new id) |
| ingested_at | when this version was exported, UTC |

### plans.csv
`plan_code`, `tier` (starter, team, business), `billing_period` (`monthly`:
the invoice covers one month; `annual`: it covers twelve months starting at
`period_start`) and the list price (informational only; invoices carry the
actual amount).

### credit_notes.csv
Credits against an invoice, in the invoice's currency. An invoice can have
several.

### accounts.csv
`account_id`, `name`, `segment` (smb, mid_market, enterprise), `country`,
`currency`, `is_internal` (1 = the vendor's own demo/staff account),
`created_at` (when the account record was created; not a billing date).

### fx_rates.csv
`usd_per_unit`: US dollars per one unit of `currency`, per `month` (YYYY-MM).

## Business definitions

1. **Current version.** When an `invoice_id` appears more than once, only the
   row with the latest `ingested_at` counts.
2. **Billable invoice.** Current status `paid` or `open`. Draft and void
   invoices count nowhere, and neither do their credit notes. Internal
   accounts are excluded from every figure.
3. **Net invoice amount** = `amount` minus all credit notes of the invoice.
4. **USD.** Convert the net invoice amount at the rate of the month of
   `period_start` (for annual invoices too: one rate for all twelve months).
5. **MRR of a month** = the sum over billable invoices whose service period
   covers that month of: the net USD amount for monthly plans; one twelfth of
   the net USD amount for annual plans (spread evenly over the twelve months
   from `period_start`, including months before 2026 or after the data ends).
6. **Active account in month M**: its MRR in M is greater than zero.
7. **Churned in M**: active in the month before M and not active in M.
8. **New account in M**: active in M and not active in any earlier month.
   An account's tier in a month is the tier of the plan of its billable
   invoice covering that month.
9. **ARPA** of a group in M = the group's MRR in M divided by its number of
   active accounts in M.

Round only final answers.
"""


def ld02():
    files = ld02_generate()
    emit_stub("ld02", files)
    data = load("ld02", list(files))
    a = ld02_compute(data, set())
    top(a["_grow"], 1500)
    qs = [
        dict(text="What was the total MRR in USD in June 2026?",
             fmt="USD with 2 decimals, no currency sign, e.g. `Q1: 98765.43`.", match="number",
             expect=a["Q1"], tol=1.0, dec=2,
             desc="June MRR: annual spread /12 incl. 2025 annual invoices, credits, FX at period_start month, void/draft/internal excluded"),
        dict(text="How many accounts were active in March 2026?",
             fmt="an integer.", match="number", expect=a["Q2"], tol=0,
             desc="active count: annual accounts invoiced in 2025 still active; fully credited invoices make MRR 0"),
        dict(text="How many accounts churned in April 2026?",
             fmt="an integer.", match="number", expect=a["Q3"], tol=0,
             desc="churn: active March, not April; void+reissue and re-sent status changes"),
        dict(text="Net revenue retention: take the accounts active in January 2026; what is their "
                  "combined MRR in June 2026 as a percentage of their combined MRR in January 2026?",
             fmt="a percentage with 2 decimals, e.g. `Q4: 101.37`.", match="number",
             expect=a["Q4"], tol=0.02, dec=2,
             desc="NRR cohort: fixed January cohort, churned members count as 0 in June"),
        dict(text="Which country (accounts.csv `country`) had the largest absolute MRR increase "
                  "in USD from January 2026 to June 2026?",
             fmt="the two-letter country code as in accounts.csv.", match="text", expect=a["Q5"],
             desc="country growth: Jan vs Jun MRR per country, internal accounts and FX matter"),
        dict(text="What was the ARPA in USD of the enterprise segment in April 2026?",
             fmt="USD with 2 decimals.", match="number", expect=a["Q6"], tol=0.05, dec=2,
             desc="enterprise ARPA: MRR / active enterprise accounts, annual spread"),
        dict(text="How many new accounts were there in Q2 2026 (new in April, May or June)?",
             fmt="an integer.", match="number", expect=a["Q7"], tol=0,
             desc="new accounts: reactivated accounts (paused, then back) are not new; annual accounts count once"),
        dict(text="What share of the June 2026 MRR came from annual plans?",
             fmt="a percentage with 2 decimals, e.g. `Q8: 31.40`.", match="number",
             expect=a["Q8"], tol=0.02, dec=2,
             desc="annual share: needs annual spread over 12 months including invoices from mid-2025"),
    ]
    emit("ld02", "SaaS MRR and churn audit", "SaaS subscription billing", files, LD02_README,
         qs, "All questions use the definitions in README.md. Months are calendar months.")
    trap_report("ld02", ld02_compute, data,
                ["dedup", "credits", "void", "internal", "fx", "annual", "reactivation"])


# ===================================================================== ld03
# Logistics shipments: business-day SLAs per DC calendar, cut-offs, rate card.

LD03_DCS = [
    ("AMS1", "Amsterdam", "NL", 2, "17:00"),
    ("ATL1", "Atlanta", "US", -4, "15:00"),
    ("SIN1", "Singapore", "SG", 8, "16:00"),
    ("MAD1", "Madrid", "ES", 2, "18:00"),
]
LD03_HOLIDAYS = [
    ("ATL1", "2026-07-03", "Independence Day (observed)"),
    ("ATL1", "2026-09-07", "Labor Day"),
    ("SIN1", "2026-08-10", "National Day (observed)"),
    ("AMS1", "2026-08-21", "Warehouse inventory day"),
    ("MAD1", "2026-08-14", "Regional holiday (bridge day)"),
    ("MAD1", "2026-08-15", "Assumption Day"),
    ("ATL1", "2026-11-26", "Thanksgiving"),
]
LD03_CARRIERS = [
    ("DHX", "DHL Express", 1), ("FDX", "FedEx Economy", 3), ("PNL", "PostNL Parcel", 2),
    ("SGP", "SingPost Speedpost", 3), ("UPG", "UPS Ground", 4),
]
LD03_DC_CARRIERS = {"AMS1": ["PNL", "DHX", "UPG"], "ATL1": ["UPG", "FDX", "DHX"],
                    "SIN1": ["SGP", "DHX", "FDX"], "MAD1": ["DHX", "UPG", "PNL"]}
LD03_BANDS = [(1, 2), (3, 5), (6, 10), (11, 20), (21, 40), (41, 70)]
LD03_BASE = {"DHX": 14.0, "FDX": 8.5, "PNL": 5.2, "SGP": 6.1, "UPG": 7.4}
LD03_DEST = {"AMS1": ["NL", "BE", "DE", "FR"], "ATL1": ["US", "CA", "MX"],
             "SIN1": ["SG", "MY", "ID", "TH"], "MAD1": ["ES", "PT", "ES"]}
# probability of 0/+1/+2/+3 business days over SLA, and of early (-1)
LD03_LATE = {"DHX": (0.10, 0.07, 0.02, 0.0), "FDX": (0.06, 0.03, 0.01, 0.25),
             "PNL": (0.08, 0.04, 0.01, 0.2), "SGP": (0.09, 0.05, 0.01, 0.15),
             "UPG": (0.05, 0.02, 0.01, 0.35)}
LD03_CUTOFF = DT(2026, 10, 1)


def ld03_calendar(holiday_rows):
    hol = {}
    for r in holiday_rows:
        hol.setdefault(r["dc_id"], set()).add(pdate(r["date"]))

    def business(dc, d):
        return d.weekday() < 5 and d not in hol.get(dc, ())

    def next_business(dc, d):
        d = d + TD(days=1)
        while not business(dc, d):
            d += TD(days=1)
        return d
    return business, next_business


def ld03_ship_date(dc_row, shipped_utc, business, next_business, naive=()):
    off = int(dc_row["utc_offset_hours"])
    local = shipped_utc + TD(hours=off) if "tz" not in naive else shipped_utc
    d = local.date()
    hh, mm = map(int, dc_row["cutoff_local"].split(":"))
    if "cutoff" not in naive and (local.hour, local.minute) >= (hh, mm):
        d = next_business(dc_row["dc_id"], d)
    if "cutoff" not in naive and not business(dc_row["dc_id"], d):
        d = next_business(dc_row["dc_id"], d)
    return d


def ld03_generate():
    rng = random.Random(82603)
    dcs = {d[0]: {"dc_id": d[0], "utc_offset_hours": str(d[3]), "cutoff_local": d[4]}
           for d in LD03_DCS}
    business, next_business = ld03_calendar(
        [{"dc_id": h[0], "date": h[1]} for h in LD03_HOLIDAYS])
    sla = {c[0]: c[2] for c in LD03_CARRIERS}
    rows = []
    sid = 700001
    start = DT(2026, 6, 28)
    for _ in range(4300):
        dc = rng.choices(["AMS1", "ATL1", "SIN1", "MAD1"], weights=[35, 30, 20, 15])[0]
        carrier = rng.choices(LD03_DC_CARRIERS[dc], weights=[50, 25, 25])[0]
        off = int(dcs[dc]["utc_offset_hours"])
        day = start + TD(days=rng.randint(0, 94))
        if day.weekday() == 6 and rng.random() < 0.7:
            day += TD(days=1)
        local = day + TD(hours=rng.randint(6, 21), minutes=rng.randint(0, 59), seconds=rng.randint(0, 59))
        shipped = local - TD(hours=off)
        if shipped >= LD03_CUTOFF - TD(hours=6):
            continue
        sdate = ld03_ship_date(dcs[dc], shipped, business, next_business)
        p0, p1, p2, early = LD03_LATE[carrier]
        r = rng.random()
        delta = 3 if r < p2 / 2 else 2 if r < p2 else 1 if r < p2 + p1 else 0
        if r > p0 + p1 + p2:
            delta = 0
        if delta == 0 and rng.random() < early and sla[carrier] > 1:
            delta = -rng.randint(1, sla[carrier] - 1)
        transit = sla[carrier] + delta
        dest = rng.choice(LD03_DEST[dc])
        if dest == "MX" and rng.random() < 0.12:
            transit += 1  # customs hold
        d = sdate
        for _ in range(transit):
            d = next_business(dc, d)
        if d.weekday() == 4 and rng.random() < 0.15:
            d += TD(days=1)  # Saturday delivery
        dlocal = DT(d.year, d.month, d.day, rng.randint(8, 20), rng.randint(0, 59))
        delivered = dlocal - TD(hours=off)
        if delivered <= shipped:
            delivered = shipped + TD(hours=rng.randint(3, 8))
        weight = round(rng.lognormvariate(1.3, 0.8), 1)
        weight = min(max(weight, 0.1), 38.0)
        dims = [rng.randint(10, 60), rng.randint(10, 45), rng.randint(5, 40)]
        u = rng.random()
        if u < 0.03:
            status, delivered_s = "cancelled", ""
        elif delivered >= LD03_CUTOFF or u < 0.045:
            status, delivered_s = "in_transit", ""
        elif u < 0.07:
            status, delivered_s = "returned", iso(delivered)
        else:
            status, delivered_s = "delivered", iso(delivered)
        base = ["SH%d" % sid, dc, carrier, dest, iso(shipped)]
        sid += 1
        tail = ["%.1f" % weight] + dims
        if status in ("delivered", "returned") and rng.random() < 0.4:
            rows.append(base + ["", "in_transit"] + tail + [iso(shipped + TD(hours=rng.randint(1, 20)))])
        if status == "cancelled" and rng.random() < 0.5:
            rows.append(base + ["", "in_transit"] + tail + [iso(shipped + TD(minutes=rng.randint(5, 50)))])
        upd = (delivered if delivered_s else shipped) + TD(minutes=rng.randint(1, 240))
        if status == "in_transit":
            upd = shipped + TD(hours=rng.randint(1, 30))
        if rng.random() < 0.05:
            # a weight correction re-sent later
            rows.append(base + [delivered_s, status] + tail + [iso(upd)])
            w2 = min(round(weight * rng.uniform(1.2, 2.5), 1), 64.0)
            tail = ["%.1f" % w2] + dims
            upd = upd + TD(hours=rng.randint(2, 48))
        rows.append(base + [delivered_s, status] + tail + [iso(upd)])
    rng.shuffle(rows)
    rows.sort(key=lambda r: r[4][:10])
    rate = []
    for c in LD03_CARRIERS:
        for i, (lo, hi) in enumerate(LD03_BANDS):
            rate.append([c[0], lo, hi, "%.2f" % (LD03_BASE[c[0]] * (1 + 0.55 * i) + 0.35 * i * i)])
    files = {
        "shipments.csv": (["shipment_id", "dc_id", "carrier_code", "dest_country", "shipped_at_utc",
                           "delivered_at_utc", "status", "weight_kg", "length_cm", "width_cm",
                           "height_cm", "updated_at"], rows),
        "dcs.csv": (["dc_id", "city", "country", "utc_offset_hours", "cutoff_local"],
                    [list(d) for d in LD03_DCS]),
        "dc_holidays.csv": (["dc_id", "date", "name"], [list(h) for h in LD03_HOLIDAYS]),
        "carriers.csv": (["carrier_code", "name", "sla_business_days"], [list(c) for c in LD03_CARRIERS]),
        "rate_card.csv": (["carrier_code", "min_billable_kg", "max_billable_kg", "price_eur"], rate),
    }
    return files


def ld03_compute(data, naive):
    dcs = {d["dc_id"]: d for d in data["dcs.csv"]}
    sla = {c["carrier_code"]: int(c["sla_business_days"]) for c in data["carriers.csv"]}
    business, next_business = ld03_calendar([] if "holidays" in naive else data["dc_holidays.csv"])
    rates = {}
    for r in data["rate_card.csv"]:
        rates.setdefault(r["carrier_code"], []).append(
            (int(r["min_billable_kg"]), int(r["max_billable_kg"]), float(r["price_eur"])))
    if "dedup" in naive:
        cur = first_kept(data["shipments.csv"], "shipment_id")
    else:
        cur = latest(data["shipments.csv"], "shipment_id", "updated_at")
    recs = []
    for s in cur.values():
        dc = dcs[s["dc_id"]]
        shipped = piso(s["shipped_at_utc"])
        sd = ld03_ship_date(dc, shipped, business, next_business, naive)
        rec = {"s": s, "ship": sd, "month": ym(sd), "status": s["status"], "dc": s["dc_id"],
               "carrier": s["carrier_code"], "shipped": shipped}
        if s["delivered_at_utc"]:
            off = int(dc["utc_offset_hours"]) if "tz" not in naive else 0
            dd = (piso(s["delivered_at_utc"]) + TD(hours=off)).date()
            n, d = 0, sd
            while d < dd:
                d += TD(days=1)
                if business(s["dc_id"], d) if "weekends" not in naive else True:
                    n += 1
            rec["transit"] = n
            rec["ontime"] = n <= sla[s["carrier_code"]]
        vol = int(s["length_cm"]) * int(s["width_cm"]) * int(s["height_cm"]) / 5000.0
        w = float(s["weight_kg"])
        bill = w if "volumetric" in naive else max(w, vol)
        rec["kg"] = int(math.ceil(bill - 1e-9))
        rec["kg"] = max(rec["kg"], 1)
        rec["price"] = [p for lo, hi, p in rates[s["carrier_code"]] if lo <= rec["kg"] <= hi][0]
        recs.append(rec)
    q3 = [r for r in recs if "2026-07" <= r["month"] <= "2026-09"]
    dq3 = [r for r in q3 if r["status"] == "delivered"]
    out = {}
    out["Q1"] = sum(1 for r in recs if r["status"] == "delivered" and r["month"] == "2026-08")
    dhx = [r for r in dq3 if r["carrier"] == "DHX"]
    out["Q2"] = r2(100.0 * sum(r["ontime"] for r in dhx) / len(dhx))
    rate_by = {}
    for c in sla:
        xs = [r for r in dq3 if r["carrier"] == c]
        rate_by[c] = 100.0 * sum(r["ontime"] for r in xs) / len(xs)
    out["Q3"] = sorted(rate_by.items(), key=lambda kv: (kv[1], kv[0]))[0][0]
    out["Q4"] = r2(sum(r["price"] for r in recs if r["dc"] == "SIN1" and r["month"] == "2026-09"
                       and r["status"] != "cancelled"))
    atl = [r["transit"] for r in dq3 if r["dc"] == "ATL1"]
    out["Q5"] = r2(float(sum(atl)) / len(atl))
    late = {}
    for r in dq3:
        if not r["ontime"]:
            late[r["s"]["dest_country"]] = late.get(r["s"]["dest_country"], 0) + 1
    out["Q6"] = sorted(late.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    out["Q7"] = sum(1 for r in recs if r["status"] == "in_transit" and r["shipped"] < DT(2026, 9, 20))
    out["Q8"] = sum(r["kg"] for r in q3 if r["carrier"] == "UPG" and r["status"] != "cancelled")
    out["_rate"] = rate_by
    out["_late"] = late
    return out


LD03_README = """
# Outbound shipments: data dictionary

Parcel shipments from four distribution centres (DCs), shipped from late June
to the end of September 2026; the export was taken at 2026-10-01T00:00:00Z.
Comma-separated, header row.

## Files

### shipments.csv
One row per shipment **update**: the tracking feed re-sends a shipment
whenever something about it changes (status, delivery scan, corrected weight).

| column | meaning |
| -- | -- |
| shipment_id | shipment identifier |
| dc_id | shipping DC, see dcs.csv |
| carrier_code | see carriers.csv |
| dest_country | destination country code |
| shipped_at_utc | when the parcel was handed over at the DC dock, UTC |
| delivered_at_utc | delivery scan, UTC; empty while not delivered |
| status | `in_transit`, `delivered`, `returned` (delivered back to the DC; not a delivery), `cancelled` (label voided, never billed) |
| weight_kg | actual weight |
| length_cm, width_cm, height_cm | parcel dimensions |
| updated_at | when this update was recorded, UTC |

### dcs.csv
`utc_offset_hours`: the DC's fixed offset from UTC for this period (local =
UTC + offset). `cutoff_local`: the daily dock cut-off in DC-local time.

### dc_holidays.csv
Days on which a DC and its carriers do not work (per DC).

### carriers.csv
`sla_business_days`: the promised transit time.

### rate_card.csv
Price per parcel in EUR by carrier and billable-weight band; bands are
inclusive on both ends and in whole kilograms.

## Business definitions

1. **Current state.** When a `shipment_id` appears more than once, only the
   row with the latest `updated_at` counts.
2. **Business day** of a DC: Monday to Friday, except that DC's holidays.
3. **Ship date.** The DC-local date of `shipped_at_utc`. If the local time is
   at or after the DC's cut-off, or that date is not a business day of the
   DC, the ship date is the next business day of the DC. Every month or
   quarter filter on shipments uses the ship date.
4. **Delivery date.** The DC-local date of `delivered_at_utc` (the DC's
   offset is used for the destination too).
5. **Transit days** = the number of business days of the DC after the ship
   date up to and including the delivery date (delivered on the ship date =
   0; a Saturday delivery counts like the Friday before).
6. **On time**: transit days <= the carrier's `sla_business_days`. On-time
   rates are over shipments whose current status is `delivered`.
7. **Billable weight** = the greater of `weight_kg` and the volumetric weight
   length x width x height / 5000 (cm, giving kg), rounded **up** to a whole
   kilogram (minimum 1 kg). The price is the carrier's band containing the
   billable weight.
8. **Freight cost** is charged for every shipment except `cancelled` ones.

Round only final answers.
"""


def ld03():
    files = ld03_generate()
    emit_stub("ld03", files)
    data = load("ld03", list(files))
    a = ld03_compute(data, set())
    bottom(a["_rate"], 1.5)
    top(a["_late"], 3)
    qs = [
        dict(text="How many shipments with a ship date in August 2026 have the current status "
                  "`delivered`?",
             fmt="an integer.", match="number", expect=a["Q1"], tol=0,
             desc="Aug delivered: latest update wins, ship date with cut-off/holiday roll and DC timezone"),
        dict(text="What was the on-time rate of carrier DHX in Q3 2026 (ship dates July to "
                  "September), as a percentage?",
             fmt="a percentage with 2 decimals, e.g. `Q2: 88.10`.", match="number",
             expect=a["Q2"], tol=0.02, dec=2,
             desc="DHX on-time: 1-day SLA, business days per DC calendar incl. holidays, Saturday deliveries"),
        dict(text="Which carrier had the lowest on-time rate in Q3 2026?",
             fmt="the carrier_code.", match="text", expect=a["Q3"],
             desc="worst carrier: per-carrier on-time over delivered Q3 shipments"),
        dict(text="What was the total freight cost in EUR of DC SIN1's shipments with a ship date "
                  "in September 2026?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q4"], tol=0.5, dec=2,
             desc="SIN1 freight: volumetric billable weight rounded up, inclusive bands, returned/in-transit billed, cancelled not"),
        dict(text="What was the average number of transit days of the delivered shipments of DC "
                  "ATL1 with a ship date in Q3 2026?",
             fmt="a number with 2 decimals.", match="number", expect=a["Q5"], tol=0.011, dec=2,
             desc="ATL1 transit: US holidays (Jul 3, Sep 7), UTC-4 dates, 15:00 cut-off"),
        dict(text="Which destination country had the most late deliveries (delivered, not on "
                  "time) among shipments with a ship date in Q3 2026? Ties go to the "
                  "alphabetically first code.",
             fmt="the dest_country code.", match="text", expect=a["Q6"],
             desc="late by country: on-time rule applied per shipment"),
        dict(text="How many shipments are still in transit (current status `in_transit`) although "
                  "they were shipped (shipped_at_utc) before 2026-09-20T00:00:00Z?",
             fmt="an integer.", match="number", expect=a["Q7"], tol=0,
             desc="stuck parcels: only the latest update counts; earlier in_transit rows of delivered parcels are noise"),
        dict(text="What was the total billable weight in kg of carrier UPG's shipments with a ship "
                  "date in Q3 2026 (cancelled excluded)?",
             fmt="an integer (kg).", match="number", expect=a["Q8"], tol=0,
             desc="UPG billable kg: volumetric max then ceil per parcel, corrected weights from later updates"),
    ]
    emit("ld03", "Shipment SLA and freight audit", "outbound logistics", files, LD03_README, qs,
         "All questions use the definitions in README.md. Q3 2026 = ship dates 2026-07-01 to "
         "2026-09-30.")
    trap_report("ld03", ld03_compute, data,
                ["dedup", "holidays", "tz", "cutoff", "weekends", "volumetric"])


# ===================================================================== ld04
# Payroll: timesheets, effective-dated rates, weekly overtime, holidays.

LD04_DEPTS = [("D10", "Assembly", "CC-100"), ("D20", "Logistics", "CC-200"),
              ("D30", "Engineering", "CC-300"), ("D40", "Customer Service", "CC-400"),
              ("D50", "Quality", "CC-500")]
LD04_HOLIDAYS = [
    ("NL", "2026-07-01", "Keti Koti (company holiday)"), ("NL", "2026-12-25", "Christmas Day"),
    ("BE", "2026-07-21", "Belgian National Day"), ("BE", "2026-08-15", "Assumption Day"),
    ("BE", "2026-11-11", "Armistice Day"), ("DE", "2026-10-03", "German Unity Day"),
    ("DE", "2026-08-15", "Assumption Day (Bavaria)"), ("DE", "2026-09-21", "Company day off (DE sites)"),
]
LD04_PROJECTS = ["PRJ-ORION", "PRJ-VEGA", "PRJ-LYRA", "OPS", "ADMIN"]
LD04_START, LD04_END = D(2026, 6, 29), D(2026, 10, 4)


def ld04_generate():
    rng = random.Random(82604)
    hol = {(h[0], pdate(h[1])) for h in LD04_HOLIDAYS}
    employees, rates, entries = [], [], []
    n = 96
    for i in range(1, n + 1):
        eid = "E%03d" % i
        dept = rng.choices([d[0] for d in LD04_DEPTS], weights=[34, 25, 15, 18, 8])[0]
        country = rng.choices(["NL", "BE", "DE"], weights=[50, 25, 25])[0]
        pay = "hourly" if rng.random() < (0.85 if dept in ("D10", "D20") else 0.45) else "salaried"
        hire = D(2019, 1, 1) + TD(days=rng.randint(0, 2700))
        if rng.random() < 0.06:
            hire = D(2026, 7, 1) + TD(days=rng.randint(0, 70))
        term = ""
        if rng.random() < 0.08:
            term = (D(2026, 7, 10) + TD(days=rng.randint(0, 70))).isoformat()
        if i == 17:
            dept, country, pay, hire, term = "D20", "BE", "hourly", D(2021, 3, 1), ""
        employees.append([eid, "Employee %s" % eid[1:], dept, country, pay, hire.isoformat(), term])
        rate = round(rng.uniform(17.5, 34.0) * (1.25 if pay == "salaried" else 1.0), 2)
        rates.append([eid, max(hire, D(2025, 1, 1)).isoformat(), "%.2f" % rate])
        if rng.random() < 0.4 or i == 17:
            eff = D(2026, 7, 1) + TD(days=rng.randint(0, 85))
            if i == 17:
                eff = D(2026, 7, 13)
            rate = round(rate * rng.uniform(1.03, 1.09), 2)
            rates.append([eid, eff.isoformat(), "%.2f" % rate])
        if rng.random() < 0.15:
            rates.append([eid, "2026-10-01", "%.2f" % round(rate * 1.04, 2)])
        # timesheet entries
        overtimer = pay == "hourly" and (rng.random() < 0.45 or i == 17)
        d = LD04_START
        leave = set()
        if rng.random() < 0.6:
            s0 = LD04_START + TD(days=rng.randint(0, 80))
            leave = {s0 + TD(days=k) for k in range(rng.choice([5, 10, 14]))}
        tdate = pdate(term) if term else None
        while d <= LD04_END:
            if d < hire or d in leave:
                d += TD(days=1)
                continue
            if tdate and d > tdate and not (i % 3 == 0 and d <= tdate + TD(days=4)):
                d += TD(days=1)
                continue
            work = d.weekday() < 5 or (overtimer and d.weekday() == 5 and rng.random() < 0.3)
            if (country, d) in hol and rng.random() < 0.75 and i != 17:
                work = False
            if work and rng.random() < 0.96:
                hours = 8.0
                if overtimer and d.weekday() < 5:
                    hours += rng.choice([0, 0, 0.5, 1, 1.5, 2, 2.5])
                if d.weekday() == 5:
                    hours = rng.choice([4.0, 5.0, 6.0])
                hours -= rng.choice([0, 0, 0, 0.25, 0.5])
                split = rng.random() < 0.35
                chunks = [hours] if not split else [hours - 2.0, 2.0]
                for h in chunks:
                    proj = rng.choice(LD04_PROJECTS)
                    if rng.random() < 0.1:
                        proj = rng.choice([proj.lower(), proj.title(), proj + " ", " " + proj])
                    st = rng.choices(["approved", "submitted", "rejected"], weights=[93, 4, 3])[0]
                    sub = DT(d.year, d.month, d.day, 17) + TD(days=rng.randint(0, 6), minutes=rng.randint(0, 600))
                    entries.append([None, eid, d.isoformat(), "%.2f" % h, proj, st, iso(sub)])
            d += TD(days=1)
    rng.shuffle(entries)
    entries.sort(key=lambda e: e[2])
    out = []
    for k, e in enumerate(entries, 1):
        e[0] = "T%06d" % k
        if rng.random() < 0.06:
            first = list(e)
            first[3] = "%.2f" % (float(e[3]) + rng.choice([-2, -1, 1, 2, 3]))
            first[5] = rng.choice(["submitted", "approved"])
            later = list(e)
            later[6] = iso(piso(e[6]) + TD(hours=rng.randint(2, 96)))
            if rng.random() < 0.2:
                later[5] = "rejected"
            out.append(first)
            out.append(later)
        else:
            out.append(e)
    rng.shuffle(out)
    out.sort(key=lambda e: e[2])
    files = {
        "timesheets.csv": (["entry_id", "employee_id", "work_date", "hours", "project_code", "status",
                            "submitted_at"], out),
        "employees.csv": (["employee_id", "name", "dept_id", "country", "pay_type", "hire_date",
                           "termination_date"], employees),
        "pay_rates.csv": (["employee_id", "effective_from", "hourly_rate_eur"], rates),
        "departments.csv": (["dept_id", "name", "cost_center"], [list(x) for x in LD04_DEPTS]),
        "public_holidays.csv": (["country", "date", "name"], [list(h) for h in LD04_HOLIDAYS]),
    }
    return files


def ld04_compute(data, naive):
    emps = {e["employee_id"]: e for e in data["employees.csv"]}
    depts = {d["dept_id"]: d["name"] for d in data["departments.csv"]}
    hol = {(h["country"], pdate(h["date"])) for h in data["public_holidays.csv"]}
    hist = {}
    for r in data["pay_rates.csv"]:
        hist.setdefault(r["employee_id"], []).append((pdate(r["effective_from"]), float(r["hourly_rate_eur"])))
    for v in hist.values():
        v.sort()

    def rate(e, d):
        if "rates" in naive:
            return hist[e][0][1]
        best = None
        for eff, r in hist[e]:
            if eff <= d:
                best = r
        return best

    if "dedup" in naive:
        cur = first_kept(data["timesheets.csv"], "entry_id")
    else:
        cur = latest(data["timesheets.csv"], "entry_id", "submitted_at")
    valid = []
    after_term = set()
    for t in cur.values():
        if t["status"] != "approved":
            continue
        e = emps[t["employee_id"]]
        d = pdate(t["work_date"])
        if e["termination_date"] and d > pdate(e["termination_date"]):
            after_term.add(e["employee_id"])
            if "termination" not in naive:
                continue
        h = float(t["hours"])
        is_hol = (e["country"], d) in hol and "holiday" not in naive
        r = rate(e["employee_id"], d)
        valid.append({"e": e["employee_id"], "d": d, "h": h, "hol": is_hol, "rate": r,
                      "proj": t["project_code"].strip().upper() if "proj_case" not in naive else t["project_code"],
                      "dept": e["dept_id"], "hourly": e["pay_type"] == "hourly",
                      "pay": h * r * (2.0 if is_hol else 1.0)})
    weeks = {}
    for v in valid:
        if v["hourly"] and not v["hol"]:
            sun = v["d"] + TD(days=6 - v["d"].weekday())
            weeks[(v["e"], sun)] = weeks.get((v["e"], sun), 0) + v["h"]
    ot = []  # (emp, sunday, ot_hours, premium)
    for (e, sun), h in weeks.items():
        if h > 40:
            ot.append((e, sun, h - 40, 0.5 * (h - 40) * rate(e, sun)))

    def cost(month, pred):
        c = sum(v["pay"] for v in valid if ym(v["d"]) == month and pred(v["e"]))
        if "overtime" not in naive:
            c += sum(p for e, sun, h, p in ot if ym(sun) == month and pred(e))
        return c

    def inq3(d):
        return D(2026, 7, 1) <= d <= D(2026, 9, 30)

    out = {}
    out["Q1"] = r2(sum(v["h"] for v in valid if ym(v["d"]) == "2026-08"))
    out["Q2"] = r2(cost("2026-07", lambda e: e == "E017"))
    out["Q3"] = r2(sum(h for e, sun, h, p in ot if inq3(sun)))
    dc = {}
    for did in depts:
        dc[depts[did]] = cost("2026-09", lambda e, did=did: emps[e]["dept_id"] == did)
    out["Q4"] = sorted(dc.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    out["Q5"] = len(after_term)
    out["Q6"] = r2(sum(v["h"] for v in valid if v["proj"] == "PRJ-ORION" and inq3(v["d"])))
    day = D(2026, 9, 1)
    act = [e for e in emps.values() if e["pay_type"] == "hourly" and pdate(e["hire_date"]) <= day
           and (not e["termination_date"] or pdate(e["termination_date"]) >= day)]
    out["Q7"] = r2(sum(rate(e["employee_id"], day) for e in act) / len(act))
    out["Q8"] = r2(sum(v["h"] * v["rate"] for v in valid if v["hol"] and inq3(v["d"])))
    out["_dc"] = dc
    return out


LD04_README = """
# Payroll costing: data dictionary

Timesheets of the operations staff for the weeks from Monday 2026-06-29 to
Sunday 2026-10-04, used for labour costing. Comma-separated, header row.

## Files

### timesheets.csv
One row per timesheet entry **version**: an entry is re-submitted under the
same `entry_id` when the employee or a manager corrects it.

| column | meaning |
| -- | -- |
| entry_id | entry identifier |
| employee_id | see employees.csv |
| work_date | the day worked |
| hours | hours worked (quarter hours) |
| project_code | project booked; codes are case-insensitive and may carry stray spaces |
| status | `approved`, `submitted` (awaiting approval) or `rejected` |
| submitted_at | when this version was submitted, UTC |

### employees.csv
`dept_id` (see departments.csv), `country` (the employee's work country, for
public holidays), `pay_type` (`hourly` or `salaried`), `hire_date`,
`termination_date` (last day of employment; empty while employed).

### pay_rates.csv
Effective-dated hourly rates (EUR). The rate in effect on a day is the row
of that employee with the latest `effective_from` on or before that day.
Salaried staff have a costing rate too.

### departments.csv, public_holidays.csv
Department names; public holidays per country.

## Business definitions

1. **Current version.** When an `entry_id` appears more than once, only the
   row with the latest `submitted_at` counts.
2. **Payable entry.** Current status `approved`, and the work date is not
   after the employee's termination date (entries after it are data errors
   and are excluded from every figure except where a question asks for them).
3. **Base pay** of a payable entry = hours x the rate in effect on the work
   date; hours worked on a public holiday of the employee's country are paid
   double (2 x the rate). The extra 1 x on holiday hours is the **holiday
   premium**.
4. **Overtime** applies to `hourly` employees only. Per employee and per week
   (Monday to Sunday), overtime hours = payable hours of the week that are
   not holiday hours, minus 40, if positive. The **overtime premium** =
   0.5 x overtime hours x the rate in effect on that week's Sunday. A week
   belongs to the month (and quarter) of its Sunday.
5. **Labour cost** of a month = base pay of the payable entries dated in that
   month + the overtime premiums of the weeks belonging to that month.

Round only final answers.
"""


def ld04():
    files = ld04_generate()
    emit_stub("ld04", files)
    data = load("ld04", list(files))
    a = ld04_compute(data, set())
    top(a["_dc"], 1000)
    qs = [
        dict(text="How many payable hours have a work date in August 2026?",
             fmt="hours with 2 decimals, e.g. `Q1: 12345.50`.", match="number", expect=a["Q1"],
             tol=0.01, dec=2,
             desc="Aug hours: latest version per entry_id, approved only, post-termination entries out"),
        dict(text="What was the labour cost in EUR of employee E017 in July 2026?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q2"], tol=0.05, dec=2,
             desc="E017 July: rate change on 2026-07-13, BE holiday 07-21 at 2x, OT weeks by Sunday (week of Jun 29 belongs to July, week ending Aug 2 to August)"),
        dict(text="How many overtime hours were there in total in the weeks belonging to Q3 2026 "
                  "(July to September)?",
             fmt="hours with 2 decimals.", match="number", expect=a["Q3"], tol=0.01, dec=2,
             desc="OT hours: hourly only, weekly >40 excluding holiday hours, weeks by Sunday"),
        dict(text="Which department had the highest labour cost in September 2026?",
             fmt="the department name as in departments.csv.", match="text", expect=a["Q4"],
             desc="dept cost Sept: base + OT premium of weeks ending in Sept"),
        dict(text="How many employees have at least one approved entry (current version) dated "
                  "after their termination date?",
             fmt="an integer.", match="number", expect=a["Q5"], tol=0,
             desc="post-termination entries: count employees, current versions only"),
        dict(text="How many payable hours were booked on project PRJ-ORION with a work date in "
                  "Q3 2026?",
             fmt="hours with 2 decimals.", match="number", expect=a["Q6"], tol=0.01, dec=2,
             desc="project hours: case/space variants of the code, Q3 date bounds (data runs Jun 29 - Oct 4)"),
        dict(text="What was the average hourly rate in effect on 2026-09-01 over the hourly "
                  "employees employed on that day (hired on or before it, not terminated before it)?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q7"], tol=0.01, dec=2,
             desc="effective-dated rate lookup; exclude future hires, terminated, salaried; ignore 2026-10-01 rows"),
        dict(text="What was the total holiday premium in EUR for work dates in Q3 2026?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q8"], tol=0.05, dec=2,
             desc="holiday premium: per-country holidays (BE 07-21, NL 07-01, DE 09-21; Saturday holidays)"),
    ]
    emit("ld04", "Payroll labour-cost audit", "payroll and timesheet", files, LD04_README, qs,
         "All questions use the definitions in README.md. Q3 2026 = 2026-07-01 to 2026-09-30 "
         "(for weeks: weeks whose Sunday falls in that range).")
    trap_report("ld04", ld04_compute, data,
                ["dedup", "rates", "termination", "holiday", "proj_case", "overtime"])


# ===================================================================== ld05
# Outpatient clinic appointments: late cancellations, tariffs, ages.

LD05_CLINICS = [("K1", "Centrum", "Utrecht"), ("K2", "Noord", "Utrecht"),
                ("K3", "Haven", "Rotterdam"), ("K4", "Park", "Amersfoort")]
LD05_INSURERS = [("INS1", "Zorgverzekeraar Een", "A"), ("INS2", "Delta Zorg", "A"),
                 ("INS3", "Basis Verzekerd", "B"), ("INS4", "Budget Zorg", "B"),
                 ("INS5", "Expat Health", "S"), ("INS6", "Cross-border Care", "B")]
LD05_TYPES = ["new_patient", "follow_up", "procedure", "telehealth"]
LD05_FEES = {  # (type, class) -> (fee from 2026-01-01, fee from 2026-04-01)
    ("new_patient", "A"): (95.0, 99.5), ("new_patient", "B"): (82.0, 85.0), ("new_patient", "S"): (120.0, 125.0),
    ("follow_up", "A"): (62.0, 64.0), ("follow_up", "B"): (55.0, 57.5), ("follow_up", "S"): (80.0, 82.0),
    ("procedure", "A"): (180.0, 186.0), ("procedure", "B"): (160.0, 166.0), ("procedure", "S"): (240.0, 248.0),
    ("telehealth", "A"): (40.0, 41.0), ("telehealth", "B"): (35.0, 36.0), ("telehealth", "S"): (50.0, 52.0),
}
LD05_NOSHOW_FEE = 30.0


def ld05_generate():
    rng = random.Random(82605)
    clinicians = []
    specs = ["general", "cardiology", "dermatology", "orthopedics", "physiotherapy"]
    for i in range(1, 19):
        clinicians.append(["P%02d" % i, "Dr. %s" % "ABCDEFGHIJKLMNOPQRS"[i - 1], LD05_CLINICS[(i - 1) % 4][0],
                           rng.choice(specs)])
    patients = []
    for i in range(1, 1401):
        age = rng.choice([rng.randint(18, 64), rng.randint(18, 90), rng.randint(60, 70)])
        bday = D(2026, 6, 30) - TD(days=int(age * 365.25) + rng.randint(-200, 200))
        ins = rng.choices([x[0] for x in LD05_INSURERS] + [""], weights=[25, 20, 20, 15, 6, 6, 8])[0]
        patients.append(["PT%04d" % i, bday.isoformat(), ins, "%04d" % rng.randint(1000, 9999)])
    pw = [rng.paretovariate(1.6) for _ in patients]
    cw = [1.0 + 0.5 * rng.random() for _ in clinicians]
    cw[6] = 2.1  # a busy clinician
    cw[10] = 1.9
    rows = []
    aid = 300001
    for _ in range(5200):
        p = rng.choices(patients, weights=pw)[0]
        c = rng.choices(clinicians, weights=cw)[0]
        clinic = c[2]
        vt = rng.choices(LD05_TYPES, weights=[18, 50, 14, 18])[0]
        day = D(2026, 1, 1) + TD(days=rng.randint(0, 180))
        while day.weekday() >= 5:
            day = day + TD(days=1)
        if day > D(2026, 6, 30):
            day = D(2026, 6, 30) - TD(days=rng.randint(1, 4))
            while day.weekday() >= 5:
                day -= TD(days=1)
        slot = DT(day.year, day.month, day.day, rng.randint(8, 16), rng.choice([0, 15, 30, 45]))
        lead = rng.choice([rng.randint(0, 6), rng.randint(3, 30), rng.randint(14, 60)])
        if vt == "new_patient":
            lead = rng.randint(5, 45)
        booked = slot - TD(days=lead, hours=rng.randint(1, 9), minutes=rng.randint(0, 59))
        ns_p = 0.07 if clinic != "K3" else 0.12
        u = rng.random()
        cancelled = ""
        if u < ns_p:
            status = "no_show"
        elif u < ns_p + 0.13:
            status = "cancelled"
            if rng.random() < 0.35:
                cancelled = slot - TD(hours=rng.randint(1, 23), minutes=rng.randint(0, 59))
            else:
                cancelled = slot - TD(hours=rng.randint(25, 24 * 10))
            if rng.random() < 0.04:
                cancelled = slot - TD(hours=24)  # exactly 24 h: not late
            if cancelled < booked:
                cancelled = booked + TD(minutes=rng.randint(5, 60))
        elif u < ns_p + 0.135:
            status = "scheduled"  # never closed out
        else:
            status = "completed"
        base = ["AP%d" % aid, p[0], c[0], clinic, vt, slot.strftime("%Y-%m-%d %H:%M"),
                booked.strftime("%Y-%m-%d %H:%M")]
        aid += 1
        upd0 = booked - TD(hours=2)  # local -> UTC (CET/CEST approx); only ordering matters
        if rng.random() < 0.45:
            rows.append(base + ["scheduled", "", iso(upd0)])
        end = (cancelled if cancelled else slot) + TD(hours=rng.randint(1, 30))
        if rng.random() < 0.03 and status == "completed":
            # recorded as no-show first, corrected later
            rows.append(base + ["no_show", "", iso(end - TD(hours=2))])
            end = end + TD(days=rng.randint(1, 4))
        rows.append(base + [status, cancelled.strftime("%Y-%m-%d %H:%M") if cancelled else "",
                            iso(end - TD(hours=2))])
    rng.shuffle(rows)
    rows.sort(key=lambda r: r[5][:10])
    tariffs = []
    for (vt, cl), (f1, f2) in sorted(LD05_FEES.items()):
        tariffs.append([vt, cl, "2026-01-01", "%.2f" % f1])
        tariffs.append([vt, cl, "2026-04-01", "%.2f" % f2])
    tariffs.append(["procedure", "B", "2026-05-15", "171.00"])
    tariffs.append(["follow_up", "A", "2026-07-01", "66.00"])
    tariffs.sort()
    files = {
        "appointments.csv": (["appt_id", "patient_id", "clinician_id", "clinic_id", "visit_type",
                              "slot_start", "booked_at", "status", "cancelled_at", "updated_at"], rows),
        "patients.csv": (["patient_id", "birth_date", "insurer_id", "postcode"], patients),
        "clinicians.csv": (["clinician_id", "name", "clinic_id", "specialty"], clinicians),
        "clinics.csv": (["clinic_id", "name", "city"], [list(c) for c in LD05_CLINICS]),
        "insurers.csv": (["insurer_id", "name", "tariff_class"], [list(x) for x in LD05_INSURERS]),
        "tariffs.csv": (["visit_type", "tariff_class", "effective_from", "fee_eur"], tariffs),
    }
    return files


def ld05_age(birth, on):
    return on.year - birth.year - ((on.month, on.day) < (birth.month, birth.day))


def ld05_compute(data, naive):
    pats = {p["patient_id"]: p for p in data["patients.csv"]}
    ins = {i["insurer_id"]: i["tariff_class"] for i in data["insurers.csv"]}
    tar = {}
    for t in data["tariffs.csv"]:
        tar.setdefault((t["visit_type"], t["tariff_class"]), []).append(
            (pdate(t["effective_from"]), float(t["fee_eur"])))

    def fee(vt, cl, d):
        if "effective" in naive:
            return sorted(tar[(vt, cl)])[-1][1]
        best = None
        for eff, f in sorted(tar[(vt, cl)]):
            if eff <= d:
                best = f
        return best

    if "dedup" in naive:
        cur = first_kept(data["appointments.csv"], "appt_id")
    else:
        cur = latest(data["appointments.csv"], "appt_id", "updated_at")
    recs = []
    for a in cur.values():
        slot = plocal(a["slot_start"])
        st = a["status"]
        if st == "cancelled" and "late" not in naive:
            if slot - plocal(a["cancelled_at"]) < TD(hours=24):
                st = "late_cancel"
        p = pats[a["patient_id"]]
        cl = ins.get(p["insurer_id"], "S") if "selfpay" not in naive else ins.get(p["insurer_id"], "A")
        rev = 0.0
        if st == "completed":
            rev = fee(a["visit_type"], cl, slot.date())
        elif st in ("no_show", "late_cancel"):
            rev = LD05_NOSHOW_FEE
        recs.append({"a": a, "slot": slot, "month": ym(slot), "st": st, "cl": cl, "rev": rev,
                     "noshow": st in ("no_show", "late_cancel"), "p": a["patient_id"]})

    def rate(rs):
        ns = sum(1 for r in rs if r["noshow"])
        den = ns + sum(1 for r in rs if r["st"] == "completed")
        return 100.0 * ns / den

    q2 = [r for r in recs if "2026-04" <= r["month"] <= "2026-06"]
    out = {}
    out["Q1"] = r2(rate(q2))
    out["Q2"] = r2(sum(r["rev"] for r in recs if r["month"] == "2026-03"))
    cc = {}
    for r in recs:
        if r["st"] == "completed" and r["month"] == "2026-05":
            cc[r["a"]["clinician_id"]] = cc.get(r["a"]["clinician_id"], 0) + 1
    out["Q3"] = sorted(cc.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    on = D(2026, 6, 30)
    visits = {}
    for r in recs:
        if r["st"] == "completed":
            visits[r["p"]] = visits.get(r["p"], 0) + 1
    if "age" in naive:
        age = lambda b: on.year - b.year
    else:
        age = lambda b: ld05_age(b, on)
    out["Q4"] = sum(1 for p, n in visits.items() if n >= 3 and age(pdate(pats[p]["birth_date"])) >= 65)
    leads = [(r["slot"].date() - plocal(r["a"]["booked_at"]).date()).days for r in recs
             if r["st"] == "completed" and r["a"]["visit_type"] == "new_patient"]
    out["Q5"] = median(leads)
    cr = {}
    for k in sorted({r["a"]["clinic_id"] for r in recs}):
        cr[k] = rate([r for r in recs if r["a"]["clinic_id"] == k])
    out["Q6"] = sorted(cr.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    nsp = {}
    for r in recs:
        if r["noshow"]:
            nsp[r["p"]] = nsp.get(r["p"], 0) + 1
    out["Q7"] = sum(1 for n in nsp.values() if n >= 2)
    out["Q8"] = r2(sum(r["rev"] for r in q2 if r["cl"] == "S"))
    out["_cc"] = cc
    out["_cr"] = cr
    return out


LD05_README = """
# Outpatient appointments: data dictionary

Appointments of four outpatient clinics with slots from 2026-01-01 to
2026-06-30, exported on 2026-07-01. Comma-separated, header row. All local
times (`slot_start`, `booked_at`, `cancelled_at`) are clinic local time,
format `YYYY-MM-DD HH:MM`.

## Files

### appointments.csv
One row per appointment **version**: the scheduling system re-sends an
appointment whenever its status changes.

| column | meaning |
| -- | -- |
| appt_id | appointment identifier |
| patient_id | see patients.csv |
| clinician_id | see clinicians.csv |
| clinic_id | where the appointment takes place |
| visit_type | `new_patient`, `follow_up`, `procedure`, `telehealth` |
| slot_start | appointment start, local |
| booked_at | when the appointment was booked, local |
| status | `scheduled`, `completed`, `no_show` or `cancelled` |
| cancelled_at | when the patient cancelled, local (cancelled only) |
| updated_at | when this version was recorded, UTC |

### patients.csv
`birth_date`, `insurer_id` (empty = uninsured), `postcode`.

### insurers.csv
`tariff_class` of each insurer: `A`, `B` or `S` (self-pay rates).

### tariffs.csv
Fee per visit type and tariff class, effective-dated: the fee for a visit is
the row with the latest `effective_from` on or before the slot date.

### clinicians.csv, clinics.csv
Clinician and clinic master data.

## Business definitions

1. **Current version.** When an `appt_id` appears more than once, only the
   row with the latest `updated_at` counts. Appointments whose current status
   is still `scheduled` were never closed out and are ignored.
2. **Late cancellation**: a cancelled appointment whose `cancelled_at` is
   less than 24 hours before `slot_start` (exactly 24 hours is not late).
   A late cancellation is treated as a **no-show** in every figure.
3. **No-show rate** = no-shows / (completed + no-shows), no-shows including
   late cancellations. Other cancellations are left out entirely.
4. **Tariff class** of a patient = the class of their insurer; uninsured
   patients are class `S`.
5. **Billed revenue** = for each completed appointment the fee of its visit
   type and the patient's tariff class in effect on the slot date, plus a
   flat no-show fee of 30.00 EUR for each no-show (including late
   cancellations), whatever the class.
6. **Age** = completed years on 2026-06-30.
7. Months and quarters refer to the slot date.

Round only final answers.
"""


def ld05():
    files = ld05_generate()
    emit_stub("ld05", files)
    data = load("ld05", list(files))
    a = ld05_compute(data, set())
    top(a["_cc"], 2)
    top(a["_cr"], 1.0)
    qs = [
        dict(text="What was the no-show rate in Q2 2026 (slots from April to June), as a "
                  "percentage?",
             fmt="a percentage with 2 decimals, e.g. `Q1: 9.15`.", match="number", expect=a["Q1"],
             tol=0.02, dec=2,
             desc="Q2 no-show rate: late cancels (<24h, exactly 24h not late) count as no-shows; other cancels out of the denominator"),
        dict(text="What was the billed revenue in EUR for slots in March 2026?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q2"], tol=0.5, dec=2,
             desc="March revenue: tariff by patient class (uninsured = S), no-show fee incl late cancels, re-sent no_show->completed"),
        dict(text="Which clinician had the most completed appointments in May 2026? Ties go to "
                  "the lowest clinician_id.",
             fmt="the clinician_id, e.g. `Q3: P01`.", match="text", expect=a["Q3"],
             desc="top clinician May: current versions only"),
        dict(text="How many distinct patients aged 65 or over had at least 3 completed "
                  "appointments in the first half of 2026?",
             fmt="an integer.", match="number", expect=a["Q4"], tol=0,
             desc="65+ with >=3 visits: exact age on 2026-06-30 (birthday not yet reached)"),
        dict(text="What is the median booking lead time in days of the completed `new_patient` "
                  "appointments of the first half of 2026? Lead time = slot date minus the date of "
                  "booked_at, in calendar days. With an even count take the mean of the two middle "
                  "values.",
             fmt="a number with 1 decimal, e.g. `Q5: 12.0`.", match="number", expect=a["Q5"],
             tol=0.05, dec=1,
             desc="median lead time: date difference, completed new_patient only"),
        dict(text="Which clinic had the highest no-show rate in the first half of 2026?",
             fmt="the clinic_id.", match="text", expect=a["Q6"],
             desc="clinic no-show rate over H1 with late cancels"),
        dict(text="How many patients had two or more no-shows (late cancellations included) in "
                  "the first half of 2026?",
             fmt="an integer.", match="number", expect=a["Q7"], tol=0,
             desc="repeat no-shows: late cancels included, corrected no_show rows superseded"),
        dict(text="What was the billed revenue in EUR in Q2 2026 from patients of tariff class S?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q8"], tol=0.5, dec=2,
             desc="class S revenue: uninsured patients are S; April fee schedule; no-show fees included"),
    ]
    emit("ld05", "Outpatient no-show and billing audit", "outpatient clinic appointment", files,
         LD05_README, qs, "All questions use the definitions in README.md. Q2 2026 = slot dates "
         "2026-04-01 to 2026-06-30; first half = 2026-01-01 to 2026-06-30.")
    trap_report("ld05", ld05_compute, data, ["dedup", "late", "selfpay", "effective", "age"])


# ===================================================================== ld06
# Energy meter readings: meter swaps, CT multipliers, local peak windows.

LD06_SITES = [
    ("S01", "Head office", 2, "T1"), ("S02", "Warehouse North", 2, "T2"),
    ("S03", "Cold store", 2, "T2"), ("S04", "London branch", 1, "T1"),
    ("S05", "Plant", 2, "T3"), ("S06", "Boston lab", -4, "T1"), ("S07", "Campus", 2, "T2"),
]
LD06_METERS = [
    ("M01", "S01", "2023-02-01T00:00:00Z", "", 1, 30.0),
    ("M02", "S02", "2022-11-15T00:00:00Z", "", 1, 22.0),
    ("M03", "S03", "2021-06-01T00:00:00Z", "2026-09-14T09:00:00Z", 1, 48.0),
    ("M08", "S03", "2026-09-14T09:00:00Z", "", 2, 24.0),
    ("M04", "S04", "2024-03-01T00:00:00Z", "", 1, 14.0),
    ("M05", "S05", "2020-01-01T00:00:00Z", "", 40, 1.9),
    ("M06", "S06", "2025-05-01T00:00:00Z", "", 1, 18.0),
    ("M07", "S07", "2023-08-01T00:00:00Z", "", 1, 26.0),
    ("M09", "S07", "2026-09-01T00:00:00Z", "", 1, 9.0),
]
LD06_TARIFFS = [("T1", "0.3120", "0.2140", "1.85"), ("T2", "0.2890", "0.1960", "4.20"),
                ("T3", "0.2410", "0.1580", "12.50")]


def ld06_generate():
    rng = random.Random(82606)
    off = {s[0]: s[2] for s in LD06_SITES}
    rows = []
    t0, t1 = DT(2026, 8, 31, 12), DT(2026, 10, 1, 12)
    dayf = {}
    for k in range(-1, 32):
        d = D(2026, 9, 1) + TD(days=k)
        dayf[d] = rng.uniform(0.85, 1.15)
    dayf[D(2026, 9, 17)] = 1.55  # heat wave: chillers flat out
    dayf[D(2026, 9, 16)] = 1.3
    for mid, site, inst, rem, mult, base in LD06_METERS:
        inst_t = piso(inst)
        rem_t = piso(rem) if rem else None
        t = t0
        while t < t1:
            local = t + TD(hours=off[site])
            h = local.hour
            prof = 1.0
            if local.weekday() < 5 and 7 <= h < 19:
                prof = 2.1
            elif 6 <= h < 23:
                prof = 1.3
            if local.weekday() >= 5:
                prof *= 0.6
            kwh = base * prof * dayf.get(local.date(), 1.0) * rng.uniform(0.85, 1.15)
            ghost = rem_t is not None and t >= rem_t
            if t < inst_t and mid != "M08":
                t += TD(hours=1)
                continue
            if mid == "M08" and t < inst_t:
                if rng.random() < 0.9:
                    t += TD(hours=1)
                    continue
                kwh = 0.0  # commissioning test pulses before go-live
            if ghost:
                kwh = rng.uniform(0.0, 0.3)
            if mid == "M06" and rng.random() < 0.035:
                t += TD(hours=1)
                continue  # gap
            ing = t + TD(hours=1, minutes=rng.randint(5, 50))
            q = rng.choices(["A", "E", "X"], weights=[92, 6, 2])[0]
            if q == "X":
                bad = round(kwh * rng.choice([0, 0, 10, 100]), 3)
                rows.append([mid, iso(t), "%.3f" % bad, "X", iso(ing)])
                if rng.random() < 0.5 or (mid == "M06" and rng.random() < 0.3):
                    t += TD(hours=1)
                    continue  # never replaced
                ing = ing + TD(hours=rng.randint(6, 72))
                q = "E"
                kwh = kwh * rng.uniform(0.9, 1.1)
            if q == "E" and rng.random() < 0.55:
                rows.append([mid, iso(t), "%.3f" % kwh, "E", iso(ing)])
                ing = ing + TD(days=rng.randint(1, 6), minutes=rng.randint(0, 59))
                kwh = kwh * rng.uniform(0.85, 1.15)
                q = "A"
            rows.append([mid, iso(t), "%.3f" % kwh, q, iso(ing)])
            t += TD(hours=1)
    rows.sort(key=lambda r: (r[4], r[0]))
    files = {
        "readings.csv": (["meter_id", "interval_start_utc", "kwh", "quality", "ingested_at"], rows),
        "meters.csv": (["meter_id", "site_id", "installed_at_utc", "removed_at_utc", "multiplier"],
                       [list(m[:5]) for m in LD06_METERS]),
        "sites.csv": (["site_id", "name", "utc_offset_hours", "tariff_id"], [list(s) for s in LD06_SITES]),
        "tariffs.csv": (["tariff_id", "peak_rate_eur_per_kwh", "offpeak_rate_eur_per_kwh",
                         "standing_charge_eur_per_day"], [list(t) for t in LD06_TARIFFS]),
    }
    return files


def ld06_compute(data, naive):
    meters = {m["meter_id"]: m for m in data["meters.csv"]}
    sites = {s["site_id"]: s for s in data["sites.csv"]}
    tariffs = {t["tariff_id"]: t for t in data["tariffs.csv"]}
    cur = {}
    for r in data["readings.csv"]:
        k = (r["meter_id"], r["interval_start_utc"])
        if "dedup" in naive:
            cur.setdefault(k, r)
        elif k not in cur or piso(r["ingested_at"]) > piso(cur[k]["ingested_at"]):
            cur[k] = r
    recs = []
    for (mid, ts), r in cur.items():
        if r["quality"] == "X" and "invalid" not in naive:
            continue
        m = meters[mid]
        t = piso(ts)
        if "window" not in naive:
            if t < piso(m["installed_at_utc"]):
                continue
            if m["removed_at_utc"] and t >= piso(m["removed_at_utc"]):
                continue
        site = m["site_id"]
        local = t + TD(hours=int(sites[site]["utc_offset_hours"]) if "tz" not in naive else 0)
        if ym(local) != "2026-09":
            continue
        mult = int(m["multiplier"]) if "multiplier" not in naive else 1
        kwh = float(r["kwh"]) * mult
        peak = local.weekday() < 5 and 7 <= local.hour < 23
        recs.append({"site": site, "local": local, "kwh": kwh, "peak": peak, "q": r["quality"]})
    out = {}
    out["Q1"] = r2(sum(r["kwh"] for r in recs if r["site"] == "S03"))
    tot = sum(r["kwh"] for r in recs)
    out["Q2"] = r2(100.0 * sum(r["kwh"] for r in recs if r["peak"]) / tot)
    t = tariffs[sites["S05"]["tariff_id"]]
    pk = sum(r["kwh"] for r in recs if r["site"] == "S05" and r["peak"])
    op = sum(r["kwh"] for r in recs if r["site"] == "S05" and not r["peak"])
    out["Q3"] = r2(pk * float(t["peak_rate_eur_per_kwh"]) + op * float(t["offpeak_rate_eur_per_kwh"])
                   + 30 * float(t["standing_charge_eur_per_day"]))
    hourly = {}
    for r in recs:
        k = (r["site"], r["local"])
        hourly[k] = hourly.get(k, 0) + r["kwh"]
    best = {}
    for (s, _), v in hourly.items():
        best[s] = max(best.get(s, 0), v)
    out["Q4"] = sorted(best.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    have = {r["local"] for r in recs if r["site"] == "S06"}
    out["Q5"] = 720 - len(have)
    daily = {}
    for r in recs:
        d = r["local"].date().isoformat()
        daily[d] = daily.get(d, 0) + r["kwh"]
    out["Q6"] = sorted(daily.items(), key=lambda kv: (-kv[1], kv[0]))[0][0]
    out["Q7"] = r2(sum(r["kwh"] for r in recs if r["q"] == "E"))
    wk = [r["kwh"] for r in recs if r["site"] == "S01" and r["local"].weekday() >= 5]
    ndays = len({r["local"].date() for r in recs if r["site"] == "S01" and r["local"].weekday() >= 5})
    out["Q8"] = r2(sum(wk) / ndays)
    out["_best"] = best
    out["_daily"] = daily
    return out


LD06_README = """
# Site energy metering: data dictionary

Hourly electricity readings of seven sites for September 2026. The export
holds UTC intervals from 2026-08-31T12:00Z to 2026-10-01T12:00Z so that
every site's local September is covered. Comma-separated, header row.

## Files

### readings.csv
One row per reading **version**: the meter data platform re-sends an
interval when a reading is validated, estimated or corrected.

| column | meaning |
| -- | -- |
| meter_id | see meters.csv |
| interval_start_utc | start of the one-hour interval, UTC |
| kwh | register advance in the interval, in meter units (see `multiplier`) |
| quality | `A` actual, `E` estimated (a valid substitute value), `X` failed validation |
| ingested_at | when this version was loaded, UTC |

### meters.csv
`site_id` the meter belongs to; `installed_at_utc` and `removed_at_utc`
(empty = still installed): a meter's readings count only from its
installation (inclusive) to its removal (exclusive); anything a meter sends
outside that window is not site consumption. `multiplier`: the
current-transformer ratio; consumption in kWh = `kwh` x `multiplier`. A site
can have several meters at the same time; their consumption adds up.

### sites.csv
`utc_offset_hours`: the site's fixed offset from UTC for September 2026
(local = UTC + offset). `tariff_id`: see tariffs.csv.

### tariffs.csv
Energy rates per kWh (peak and off-peak) and a standing charge per day.

## Business definitions

1. **Current version.** For each (`meter_id`, `interval_start_utc`) only the
   row with the latest `ingested_at` counts.
2. **Valid reading**: current quality `A` or `E`. `X` readings count as no
   data (zero consumption, and the hour has no valid reading).
3. **Local time.** An interval belongs to the site-local date and hour of its
   start (UTC + the site's offset). "September" = local September 1-30.
4. **Peak** = intervals starting at local 07:00 up to and including 22:00 on
   Monday to Friday; everything else (nights, Saturday, Sunday) is off-peak.
5. **Energy bill** of a site for a month = peak kWh x peak rate + off-peak kWh
   x off-peak rate + standing charge x days in the month.
6. **Site hour**: the site's consumption in one local hour = the sum over its
   meters' valid readings for that interval.

Round only final answers.
"""


def ld06():
    files = ld06_generate()
    emit_stub("ld06", files)
    data = load("ld06", list(files))
    a = ld06_compute(data, set())
    top(a["_best"], 3)
    top(a["_daily"], 200)
    qs = [
        dict(text="What was the total electricity consumption in kWh of site S03 in September 2026?",
             fmt="kWh with 2 decimals.", match="number", expect=a["Q1"], tol=0.5, dec=2,
             desc="S03: meter swap M03->M08 on 09-14 09:00Z, M08 multiplier 2, M03 ghost readings and M08 pre-install pulses excluded"),
        dict(text="What share of the total consumption of all sites in September 2026 was peak "
                  "consumption?",
             fmt="a percentage with 2 decimals, e.g. `Q2: 61.25`.", match="number", expect=a["Q2"],
             tol=0.02, dec=2,
             desc="peak share: local hours per site offset, 22:00 hour inclusive, weekdays only, multipliers"),
        dict(text="What is the September 2026 energy bill in EUR of site S05?",
             fmt="EUR with 2 decimals.", match="number", expect=a["Q3"], tol=0.5, dec=2,
             desc="S05 bill: CT multiplier 40, tariff T3 peak/off-peak split, 30 days standing charge"),
        dict(text="Which site had the highest consumption in a single site hour in September 2026?",
             fmt="the site_id.", match="text", expect=a["Q4"],
             desc="max site hour: multiplier decides (S05 raw readings are small); X spikes excluded"),
        dict(text="How many local September 2026 hours of site S06 have no valid reading?",
             fmt="an integer.", match="number", expect=a["Q5"], tol=0,
             desc="S06 missing hours: UTC-4 window of 720 local hours, gaps plus current-X intervals (X later replaced by E counts as valid)"),
        dict(text="On which local date was the combined consumption of all sites highest? (Each "
                  "site's intervals go to its own local date.)",
             fmt="the date as YYYY-MM-DD.", match="text", expect=a["Q6"],
             desc="peak day: per-site local dates, all meters with multipliers"),
        dict(text="How many kWh of the September 2026 consumption of all sites come from "
                  "estimated readings (current quality `E`)?",
             fmt="kWh with 2 decimals.", match="number", expect=a["Q7"], tol=0.5, dec=2,
             desc="estimated kWh: E rows later replaced by A do not count; X replaced by E does"),
        dict(text="What was the average daily consumption in kWh of site S01 on the Saturdays and "
                  "Sundays of September 2026 (local dates)?",
             fmt="kWh with 2 decimals.", match="number", expect=a["Q8"], tol=0.05, dec=2,
             desc="S01 weekend average: local weekend dates (8 days), UTC+2 boundaries"),
    ]
    emit("ld06", "Site energy metering audit", "electricity metering", files, LD06_README, qs,
         "All questions use the definitions in README.md.")
    trap_report("ld06", ld06_compute, data, ["dedup", "invalid", "window", "tz", "multiplier"])


# ===================================================================== main

def main():
    os.makedirs(TASKS, exist_ok=True)
    for fn in (ld01, ld02, ld03, ld04, ld05, ld06):
        fn()
    print("wrote", ", ".join(sorted(n for n in os.listdir(TASKS) if n.startswith("ld"))))


if __name__ == "__main__":
    main()
