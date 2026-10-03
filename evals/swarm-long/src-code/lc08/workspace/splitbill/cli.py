"""Command line entry point.

    python3 -m splitbill.cli EXPENSES.csv [--currency EUR] [--rates RATES]
                                          [--summary]

Prints the transfers that settle the group (and with --summary the table of
who paid what first).
"""

import argparse
import collections
import sys

from .balances import BalanceError, net_balances
from .currency import CurrencyError, parse_rates
from .importer import ImportFailed, load_expenses
from .money import format_cents
from .settle import SettleError, describe, settle


def per_person(expenses):
    """OrderedDict name -> (paid, share) in cents, sorted by name."""
    paid = collections.defaultdict(int)
    share = collections.defaultdict(int)
    for expense in expenses:
        paid[expense.payer] += expense.amount
        for name, cents in expense.shares.items():
            share[name] += cents
    names = sorted(set(paid) | set(share))
    return collections.OrderedDict((n, (paid[n], share[n])) for n in names)


def by_description(expenses):
    """Total amount per description (case-insensitive), largest first."""
    totals = collections.defaultdict(int)
    labels = {}
    for expense in expenses:
        key = expense.description.strip().lower()
        labels.setdefault(key, expense.description.strip())
        totals[key] += expense.amount
    ordered = sorted(totals.items(), key=lambda item: (-item[1], item[0]))
    return [(labels[key], cents) for key, cents in ordered]


def _table(header, rows, right=()):
    widths = [len(h) for h in header]
    for row in rows:
        widths = [max(w, len(cell)) for w, cell in zip(widths, row)]
    lines = []
    for row in [header] + rows:
        cells = []
        for index, (cell, width) in enumerate(zip(row, widths)):
            cells.append(cell.rjust(width) if index in right else cell.ljust(width))
        lines.append("  ".join(cells).rstrip())
    return lines


def summary(expenses, currency="EUR"):
    """Who paid what, who used what, and the net balance per person."""
    rows = []
    for name, (paid, share) in per_person(expenses).items():
        rows.append([name, format_cents(paid), format_cents(share), format_cents(paid - share)])
    total = sum(expense.amount for expense in expenses)
    lines = ["Expenses: %d, total %s %s" % (len(expenses), format_cents(total), currency), ""]
    lines += _table(["person", "paid", "share", "balance"], rows, right=(1, 2, 3))
    return "\n".join(lines)


def _read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def main(argv=None):
    parser = argparse.ArgumentParser(prog="splitbill")
    parser.add_argument("expenses")
    parser.add_argument("--currency", default="EUR")
    parser.add_argument("--rates")
    parser.add_argument("--summary", action="store_true")
    args = parser.parse_args(argv)
    try:
        rates = parse_rates(_read(args.rates)) if args.rates else None
        expenses = load_expenses(_read(args.expenses), args.currency, rates)
        transfers = settle(net_balances(expenses))
    except (ImportFailed, CurrencyError, BalanceError, SettleError) as exc:
        sys.stderr.write("splitbill: %s\n" % exc)
        return 2
    if args.summary:
        print(summary(expenses, args.currency.upper()))
        print("")
    print(describe(transfers, args.currency.upper()) or "Everybody is settled up.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
