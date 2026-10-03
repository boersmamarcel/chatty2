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
    if sum(balances.values()) != 0:
        raise SettleError("balances add up to %d, not 0" % sum(balances.values()))
    debts = {name: -amount for name, amount in balances.items() if amount < 0}
    credits = {name: amount for name, amount in balances.items() if amount > 0}
    transfers = []
    while debts and credits:
        debtor = min(debts, key=lambda name: (-debts[name], name))
        creditor = min(credits, key=lambda name: (-credits[name], name))
        amount = min(debts[debtor], credits[creditor])
        transfers.append((debtor, creditor, amount))
        debts[debtor] -= amount
        credits[creditor] -= amount
        if debts[debtor] == 0:
            del debts[debtor]
        if credits[creditor] == 0:
            del credits[creditor]
    return transfers


def describe(transfers, currency="EUR"):
    """One line per transfer: 'carol pays alice 30.00 EUR'."""
    return "\n".join("%s pays %s %s %s" % (d, c, format_cents(cents), currency)
                     for d, c, cents in transfers)
