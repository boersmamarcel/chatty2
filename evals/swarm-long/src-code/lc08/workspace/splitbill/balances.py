"""Net balance per person.

A person's balance is what they paid minus their shares: positive means the
group owes them money, negative means they owe the group. All balances
together add up to zero.
"""

import collections


class BalanceError(ValueError):
    """Raised for an inconsistent expense."""


def net_balances(expenses, people=()):
    """OrderedDict name -> net cents, sorted by name.

    `people` lists extra names to include with a zero balance.
    """
    balances = collections.defaultdict(int)
    for name in people:
        balances[name.strip().lower()] += 0
    for expense in expenses:
        if expense.amount <= 0:
            raise BalanceError("%s: amount must be positive" % (expense.description,))
        if sum(expense.shares.values()) != expense.amount:
            raise BalanceError("%s: shares do not add up to the amount" % (expense.description,))
        balances[expense.payer] += expense.amount
        for name, share in expense.shares.items():
            balances[name] -= share
    return collections.OrderedDict(sorted(balances.items()))


def apply_payments(balances, payments):
    """Balances after settling payments (debtor, creditor, cents) were made."""
    result = collections.OrderedDict(balances)
    for debtor, creditor, cents in payments:
        result[debtor] = result.get(debtor, 0) + cents
        result[creditor] = result.get(creditor, 0) - cents
    return result
