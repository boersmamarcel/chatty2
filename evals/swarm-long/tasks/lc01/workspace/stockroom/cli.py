"""Command line: ``python3 -m stockroom <command> ...``.

Commands:

    onhand   CATALOG MOVEMENTS [--as-of DATE]     on-hand report
    value    CATALOG MOVEMENTS COSTS              stock value report (average cost)
    reorder  CATALOG MOVEMENTS [--open-po FILE]   replenishment suggestions
    expiry   CATALOG MOVEMENTS [--days N]         lots about to expire

CATALOG is the item master CSV, MOVEMENTS the movement export CSV, COSTS a
CSV ``movement_id,unit_cost`` with the receipt costs.
"""

import argparse
import csv
import datetime
import io
import sys

from . import catalog as catalog_mod
from . import io_csv, reports
from .ledger import StockLedger
from .money import to_decimal
from .reorder import build_suggestions
from .valuation import value_ledger


def _read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def _date(text):
    return datetime.datetime.strptime(text, "%Y-%m-%d").date()


def load(catalog_path, movements_path, out):
    """Load the catalog and movements; report import errors on ``out``."""
    items = catalog_mod.load_catalog(_read(catalog_path))
    result = io_csv.read_movements(_read(movements_path), items)
    for err in result.errors:
        out.write("warning: %s\n" % err)
    ledger = StockLedger(allow_negative=True)
    ledger.post_all(result.movements)
    return items, ledger


def cmd_onhand(args, out):
    items, ledger = load(args.catalog, args.movements, out)
    out.write(reports.on_hand_report(ledger, items, as_of=args.as_of) + "\n")


def cmd_value(args, out):
    items, ledger = load(args.catalog, args.movements, out)
    costs = {}
    for row in csv.DictReader(io.StringIO(_read(args.costs))):
        costs[row["movement_id"].strip()] = to_decimal(row["unit_cost"])
    books = value_ledger(ledger, costs)
    rows = [(sku, book.qty, book.avg_cost) for sku, book in sorted(books.items())]
    out.write(reports.stock_value_report(rows) + "\n")


def cmd_reorder(args, out):
    items, ledger = load(args.catalog, args.movements, out)
    today = args.today or max((m.date for m in ledger), default=datetime.date.today())
    on_hand = {sku: ledger.on_hand(sku) for sku in ledger.skus()}
    demand = {sku: ledger.daily_demand(sku, today) for sku in ledger.skus()}
    on_order = {}
    if args.open_po:
        for row in csv.DictReader(io.StringIO(_read(args.open_po))):
            sku = row["sku"].strip().upper()
            on_order[sku] = on_order.get(sku, 0) + int(row["qty"])
    suggestions = build_suggestions(items, on_hand, on_order, demand)
    out.write(reports.reorder_report(suggestions, items) + "\n")


def cmd_expiry(args, out):
    items, ledger = load(args.catalog, args.movements, out)
    today = args.today or datetime.date.today()
    lots = []
    for sku in ledger.skus():
        lots.extend(ledger.lots_for(sku))
    out.write(reports.expiry_report(lots, today, args.days) + "\n")


def build_parser():
    parser = argparse.ArgumentParser(prog="stockroom", description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command")
    for name, func in (("onhand", cmd_onhand), ("value", cmd_value),
                       ("reorder", cmd_reorder), ("expiry", cmd_expiry)):
        p = sub.add_parser(name)
        p.add_argument("catalog")
        p.add_argument("movements")
        p.set_defaults(func=func)
        if name == "onhand":
            p.add_argument("--as-of", type=_date)
        if name == "value":
            p.add_argument("costs")
        if name == "reorder":
            p.add_argument("--open-po")
            p.add_argument("--today", type=_date)
        if name == "expiry":
            p.add_argument("--days", type=int, default=30)
            p.add_argument("--today", type=_date)
    return parser


def main(argv=None, out=None):
    out = out or sys.stdout
    args = build_parser().parse_args(argv)
    if not getattr(args, "func", None):
        build_parser().print_help(out)
        return 2
    try:
        args.func(args, out)
    except (ValueError, OSError) as exc:
        sys.stderr.write("stockroom: %s\n" % exc)
        return 1
    return 0
