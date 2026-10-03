"""Trial balance.

The trial balance lists every account with its net balance in either the
debit or the credit column; the two column totals are equal when the ledger
is consistent.
"""

from .formatting import format_amount, format_table
from .money import ZERO, sum_amounts


class TrialBalanceRow(object):
    """One account line of the trial balance."""

    __slots__ = ("code", "name", "type", "debit", "credit")

    def __init__(self, code, name, type, debit, credit):
        self.code = code
        self.name = name
        self.type = type
        self.debit = debit
        self.credit = credit

    def as_tuple(self):
        return (self.code, self.name, self.debit, self.credit)

    def __repr__(self):
        return "TrialBalanceRow(%r, %s, %s)" % (self.code, self.debit, self.credit)


def build(ledger, as_of=None, include_zero=False):
    """Rows of the trial balance as of ``as_of`` (inclusive)."""
    totals = ledger.account_totals(as_of=as_of)
    rows = []
    for account in ledger.chart:
        debit, credit = totals.get(account.code, (ZERO, ZERO))
        net = debit - credit
        if net == 0 and not include_zero:
            continue
        rows.append(TrialBalanceRow(account.code, account.name, account.type,
                                    net if net > 0 else ZERO, -net if net < 0 else ZERO))
    return rows


def column_totals(rows):
    """``(total debit, total credit)`` of the rows."""
    return (sum_amounts(row.debit for row in rows), sum_amounts(row.credit for row in rows))


def is_balanced(rows):
    debit, credit = column_totals(rows)
    return debit == credit


def render(rows, currency=None):
    """Plain-text trial balance with a ``TOTAL`` row.

    Columns: Code, Account, Debit, Credit.  Amount cells are empty when the
    amount is zero (except in the total row).
    """
    def cell(amount):
        return format_amount(amount, currency) if amount != ZERO else ""

    body = [[row.code, row.name, cell(row.debit), cell(row.credit)] for row in rows]
    debit, credit = column_totals(rows)
    body.append(["", "TOTAL", format_amount(debit, currency), format_amount(credit, currency)])
    return format_table(["Code", "Account", "Debit", "Credit"], body, ["<", "<", ">", ">"])
