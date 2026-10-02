#!/usr/bin/env python3
"""Generate the ten data-audit tasks' CSV files (EV-3, AGE-670).

Run once; the output is committed and frozen with the pre-registration. The
seed is fixed, so a re-run reproduces the files byte for byte. Each task gets
`workspace/<file>.csv` (what the agent sees) and `check.json` (the known
label, which the agent never sees).

Written for Python 3.6+.
"""

import csv
import datetime
import json
import os
import random

HERE = os.path.dirname(os.path.abspath(__file__))
TASKS = os.path.join(HERE, "tasks")


def write_csv(path, header, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(header)
        for row in rows:
            w.writerow(row)


def task(name, title, file_name, question, choices, label, header, rows):
    d = os.path.join(TASKS, name)
    write_csv(os.path.join(d, "workspace", file_name), header, rows)
    prompt = (
        "%s Use the data in %s. Base every number on the file, not on a guess. "
        "End your answer with exactly one line of the form `VERDICT: <answer>`, "
        "where <answer> is one of: %s." % (question, file_name, ", ".join(choices))
    )
    with open(os.path.join(d, "task.json"), "w") as f:
        json.dump({"family": "data-audit", "title": title, "prompt": prompt}, f, indent=2)
        f.write("\n")
    with open(os.path.join(d, "check.json"), "w") as f:
        json.dump({"type": "verdict", "choices": choices, "label": label}, f, indent=2)
        f.write("\n")


def days(start, end):
    d = start
    while d <= end:
        yield d
        d += datetime.timedelta(days=1)


def d01(rng):
    # Revenue fell in September; the drop sits in one region.
    rows, oid = [], 50001
    regions = ["EU", "US", "APAC", "LATAM"]
    for day in days(datetime.date(2026, 7, 1), datetime.date(2026, 9, 30)):
        for region in regions:
            n = rng.randint(3, 6)
            if day.month == 9 and region == "APAC":
                n = rng.randint(0, 2)
            for _ in range(n):
                price = rng.choice([39.0, 99.0, 249.0])
                units = rng.randint(1, 3)
                rows.append([oid, day.isoformat(), region, units, price, round(units * price, 2)])
                oid += 1
    task("d01-region-drop", "Which region drove the September revenue drop",
         "sales.csv", "Total revenue fell from August to September. Which region accounts for most of the drop?",
         regions, "APAC", ["order_id", "date", "region", "units", "unit_price", "revenue"], rows)


def d02(rng):
    # Revenue fell in Q2 vs Q1 because the average discount rose; volume and list price are flat.
    rows, oid = [], 70001
    for day in days(datetime.date(2026, 1, 1), datetime.date(2026, 6, 30)):
        for _ in range(rng.randint(8, 12)):
            product = rng.choice(["Basic", "Plus", "Max"])
            price = {"Basic": 20.0, "Plus": 45.0, "Max": 90.0}[product]
            units = rng.randint(1, 4)
            disc = rng.choice([0, 0, 5, 10]) if day.month <= 3 else rng.choice([10, 15, 20, 25])
            rows.append([oid, day.isoformat(), product, units, price, disc,
                         round(units * price * (1 - disc / 100.0), 2)])
            oid += 1
    task("d02-drop-driver", "Volume, price or discount behind the Q2 revenue drop",
         "orders.csv",
         "Revenue in Q2 2026 is lower than in Q1 2026. Was the drop driven mainly by lower volume (units), lower list prices, or higher discounts?",
         ["VOLUME", "PRICE", "DISCOUNT"], "DISCOUNT",
         ["order_id", "date", "product", "units", "list_price", "discount_pct", "revenue"], rows)


VENDORS = ["Acme Supplies", "Brightline", "Corvus Freight", "Delta Office", "Evergreen IT", "Fulton Print"]


def d03(rng):
    # One vendor was paid twice for the same invoice number several times.
    rows, pid = [], 1
    for vendor in VENDORS:
        prefix = vendor[:3].upper()
        for i in range(60):
            inv = "%s-%04d" % (prefix, 1000 + i)
            amount = round(rng.uniform(200, 9000), 2)
            day = datetime.date(2026, 1, 1) + datetime.timedelta(days=rng.randint(0, 270))
            rows.append([pid, day.isoformat(), vendor, inv, amount])
            pid += 1
            if vendor == "Corvus Freight" and i % 12 == 5:
                later = day + datetime.timedelta(days=rng.randint(10, 40))
                rows.append([pid, later.isoformat(), vendor, inv, amount])
                pid += 1
    # Two honest repeats elsewhere: same invoice number, different amount (a credit note and its rebill).
    rows.append([pid, "2026-05-02", "Delta Office", "DEL-1007", -310.0]); pid += 1
    rows.append([pid, "2026-05-02", "Delta Office", "DEL-1007", 290.0]); pid += 1
    rng.shuffle(rows)
    task("d03-duplicate-payments", "Which vendor was paid twice for the same invoice",
         "payments.csv",
         "Audit the payments for duplicates: the same invoice paid twice for the same amount. Which vendor has duplicate payments?",
         VENDORS, "Corvus Freight", ["payment_id", "paid_on", "vendor", "invoice_no", "amount"], rows)


def benford_amount(rng):
    return round(10 ** rng.uniform(2, 5), 2)


def d04(rng):
    # 1,500 expense amounts; first digits roughly uniform (fabricated), not Benford.
    rows = []
    for i in range(1500):
        first = rng.randint(1, 9)
        mag = rng.choice([100, 1000, 10000])
        amount = round(first * mag + rng.uniform(0, mag * 0.999), 2)
        rows.append([i + 1, "E%05d" % (i + 1), amount])
    task("d04-benford", "Does the expense ledger follow Benford's law",
         "expenses.csv",
         "Check whether the leading digits of the expense amounts follow Benford's law (digit 1 about 30%, digit 9 about 5%). Does the ledger conform?",
         ["CONFORMS", "NONCONFORMS"], "NONCONFORMS", ["row", "expense_id", "amount"], rows)


def d05(rng):
    # Approval limit 5,000: one requester repeatedly splits purchases just below it.
    people = ["Ahmed", "Bianca", "Chen", "Dolores", "Emeka"]
    rows, pid = [], 1
    for day in days(datetime.date(2026, 3, 1), datetime.date(2026, 8, 31)):
        for _ in range(rng.randint(1, 3)):
            who = rng.choice(people)
            rows.append([pid, day.isoformat(), who, rng.choice(VENDORS), round(rng.uniform(80, 4600), 2)])
            pid += 1
        if day.day in (3, 17) :
            for _ in range(3):
                rows.append([pid, day.isoformat(), "Dolores", "Evergreen IT", round(rng.uniform(4700, 4990), 2)])
                pid += 1
    task("d05-split-purchases", "Who splits purchases under the approval limit",
         "purchases.csv",
         "Purchases of 5,000 or more need a second approval. Look for purchases split into several orders just below that limit (same requester, same vendor, same day). Which requester does it?",
         people, "Dolores", ["po_id", "date", "requester", "vendor", "amount"], rows)


def d06(rng):
    # Daily sales 2025 with one month absent.
    rows = []
    for day in days(datetime.date(2025, 1, 1), datetime.date(2025, 12, 31)):
        if day.month == 10:
            continue
        rows.append([day.isoformat(), rng.choice(["north", "south"]), rng.randint(20, 80), round(rng.uniform(1500, 6000), 2)])
    months = ["JANUARY", "FEBRUARY", "MARCH", "APRIL", "MAY", "JUNE", "JULY", "AUGUST",
              "SEPTEMBER", "OCTOBER", "NOVEMBER", "DECEMBER", "NONE"]
    task("d06-missing-month", "Which month is missing from the 2025 sales extract",
         "daily_sales.csv",
         "This should be a complete extract of daily sales for 2025. Is any calendar month missing entirely? Answer NONE if every month is present.",
         months, "OCTOBER", ["date", "region", "orders", "revenue"], rows)


def d07(rng):
    users = ["jkoval", "mpatel", "srossi", "tnguyen", "wbaker"]
    rows, jid = [], 1
    for day in days(datetime.date(2026, 1, 1), datetime.date(2026, 6, 30)):
        weekend = day.weekday() >= 5
        for user in users:
            n = rng.randint(1, 4)
            if weekend:
                n = 1 if (user == "srossi" and rng.random() < 0.9) else (1 if rng.random() < 0.04 else 0)
            for _ in range(n):
                rows.append([jid, day.isoformat(), user, rng.choice(["6100", "6200", "7300", "4000"]), round(rng.uniform(50, 20000), 2)])
                jid += 1
    task("d07-weekend-postings", "Which user posts journal entries at weekends",
         "journal.csv",
         "Journal entries are normally posted on working days (Monday to Friday). Which user posts journal entries at weekends far more often than the others?",
         users, "srossi", ["entry_id", "posted_on", "user", "account", "amount"], rows)


def d08(rng):
    suppliers = ["Norfield", "Oakmont", "Pinecrest", "Quayside"]
    contract = {"SKU-100": 12.50, "SKU-200": 7.25, "SKU-300": 31.00}
    rows, rid = [], 1
    for day in days(datetime.date(2026, 2, 1), datetime.date(2026, 7, 31)):
        if rng.random() < 0.7:
            for _ in range(rng.randint(1, 3)):
                sup = rng.choice(suppliers)
                sku = rng.choice(sorted(contract))
                price = contract[sku]
                if sup == "Pinecrest" and day.month >= 4:
                    price = round(price * 1.12, 2)
                rows.append([rid, day.isoformat(), sup, sku, rng.randint(10, 200), price])
                rid += 1
    task("d08-overcharge", "Which supplier invoices above the contract price",
         "invoice_lines.csv",
         "Every supplier is contracted at the same unit price per SKU: SKU-100 12.50, SKU-200 7.25, SKU-300 31.00. Which supplier invoices above the contract price?",
         suppliers, "Pinecrest", ["line_id", "invoice_date", "supplier", "sku", "quantity", "unit_price"], rows)


def d09(rng):
    plans = ["Free", "Starter", "Team", "Enterprise"]
    rows = []
    cid = 1
    for plan in plans:
        for _ in range(400):
            start = datetime.date(2025, 1, 1) + datetime.timedelta(days=rng.randint(0, 360))
            base = {"Free": 0.25, "Starter": 0.12, "Team": 0.08, "Enterprise": 0.03}[plan]
            r = rng.random()
            ended = ""
            if r < base:
                ended = (datetime.date(2026, 1, 1) + datetime.timedelta(days=rng.randint(0, 180))).isoformat()
            elif plan == "Team" and r < base + 0.22:
                ended = (datetime.date(2026, 7, 1) + datetime.timedelta(days=rng.randint(0, 91))).isoformat()
            elif r < base * 1.5:
                ended = (datetime.date(2026, 7, 1) + datetime.timedelta(days=rng.randint(0, 91))).isoformat()
            rows.append([cid, plan, start.isoformat(), ended])
            cid += 1
    task("d09-churn-spike", "Which plan's churn jumped in Q3 2026",
         "subscriptions.csv",
         "Compare cancellations (ended_on) in Q3 2026 with H1 2026, per plan, relative to the plan's size. Which plan's churn rose the most?",
         plans, "Team", ["customer_id", "plan", "started_on", "ended_on"], rows)


def d10(rng):
    stores = ["Amsterdam", "Berlin", "Copenhagen", "Dublin", "Edinburgh", "Florence"]
    rows, tid = [], 1
    for day in days(datetime.date(2026, 4, 1), datetime.date(2026, 6, 30)):
        for store in stores:
            n = {"Amsterdam": 30, "Berlin": 45, "Copenhagen": 12, "Dublin": 25, "Edinburgh": 18, "Florence": 22}[store]
            for _ in range(rng.randint(n - 5, n + 5)):
                amount = round(rng.uniform(5, 180), 2)
                rate = 0.11 if store == "Copenhagen" else 0.03
                kind = "refund" if rng.random() < rate else "sale"
                rows.append([tid, day.isoformat(), store, kind, -amount if kind == "refund" else amount])
                tid += 1
    task("d10-refund-rate", "Which store has an abnormal refund rate",
         "transactions.csv",
         "Which store's refund rate (refunds as a share of its transactions) is far above the others? Note that stores differ a lot in size.",
         stores, "Copenhagen", ["txn_id", "date", "store", "kind", "amount"], rows)


def main():
    rng = random.Random(670)
    for gen in (d01, d02, d03, d04, d05, d06, d07, d08, d09, d10):
        gen(rng)


if __name__ == "__main__":
    main()
