"""The transfers that settle all balances.

Strategy (as documented for users): repeatedly the person who owes the most
pays the person who is owed the most, as much as possible; the result is a
short list of transfers, not always the shortest possible one.
"""

from .money import format_cents


class SettleError(ValueError):
    """Raised when the balances cannot be settled."""


def settle(balances):
    """List of (debtor, creditor, cents) transfers for `balances`.

    `balances` maps name -> net cents (see `balances.net_balances`).
    """
    debtors = sorted([-amount, name] for name, amount in balances.items() if amount < 0)
    creditors = sorted([amount, name] for name, amount in balances.items() if amount > 0)
    transfers = []
    i = j = 0
    while i < len(debtors) and j < len(creditors):
        amount = min(debtors[i][0], creditors[j][0])
        transfers.append((debtors[i][1], creditors[j][1], amount))
        debtors[i][0] -= amount
        creditors[j][0] -= amount
        if debtors[i][0] == 0:
            i += 1
        if creditors[j][0] == 0:
            j += 1
    return transfers


def describe(transfers, currency="EUR"):
    """One line per transfer: 'carol pays alice 30.00 EUR'."""
    return "\n".join("%s pays %s %s %s" % (d, c, format_cents(cents), currency)
                     for d, c, cents in transfers)
