"""The Expense record and the split methods.

An expense is paid by one person and shared by one or more people. The
shares are integer cents and always add up to the expense amount. A split
specification (column `split` of the CSV import) is one of:

    equal:alice,bob,carol          equal shares
    exact:alice=10.00,bob=5.50     fixed amounts, must add up to the total
    percent:alice=60,bob=40        percentages, must add up to 100
    shares:alice=2,bob=1           proportional to whole-number weights

Names are stripped and lower-cased.
"""

import collections
from decimal import Decimal, InvalidOperation

from .money import MoneyError, parse_amount, split_even, split_weighted


class SplitError(ValueError):
    """Raised for an invalid split specification."""


class Expense(object):
    """One shared expense; `amount` and the shares are cents."""

    def __init__(self, date, payer, amount, description, shares, currency="EUR"):
        self.date = date
        self.payer = payer
        self.amount = amount
        self.description = description
        self.shares = shares
        self.currency = currency

    def __repr__(self):
        return "Expense(%r, %r, %r, %r)" % (self.date, self.payer, self.amount, self.description)


def _name(text):
    name = text.strip().lower()
    if not name:
        raise SplitError("empty name")
    return name


def split_equal(total, people):
    """OrderedDict name -> share; the people in the given order."""
    names = [_name(p) for p in people]
    if len(set(names)) != len(names):
        raise SplitError("duplicate name in %r" % (people,))
    try:
        shares = split_even(total, len(names))
    except MoneyError as exc:
        raise SplitError(str(exc))
    return collections.OrderedDict(zip(names, shares))


def split_exact(total, amounts):
    """`amounts` maps name -> cents; they must add up to `total`."""
    shares = collections.OrderedDict((_name(n), a) for n, a in amounts.items())
    if sum(shares.values()) != total:
        raise SplitError("exact shares add up to %d, not %d" % (sum(shares.values()), total))
    return shares


def split_percent(total, percents):
    """`percents` maps name -> percentage of the total."""
    shares = collections.OrderedDict()
    for name, pct in percents.items():
        shares[_name(name)] = int(round(total * float(pct) / 100))
    return shares


def split_shares(total, weights):
    """`weights` maps name -> whole-number weight."""
    names = [_name(n) for n in weights]
    try:
        values = split_weighted(total, list(weights.values()))
    except MoneyError as exc:
        raise SplitError(str(exc))
    return collections.OrderedDict(zip(names, values))


def _pairs(body):
    pairs = collections.OrderedDict()
    for item in body.split(","):
        if "=" not in item:
            raise SplitError("expected name=value, got %r" % (item,))
        name, _, value = item.partition("=")
        name = _name(name)
        if name in pairs:
            raise SplitError("duplicate name %r" % (name,))
        pairs[name] = value.strip()
    return pairs


def parse_split(spec, total):
    """Shares (OrderedDict name -> cents) of a split specification."""
    method, sep, body = spec.partition(":")
    method = method.strip().lower()
    if not sep or not body.strip():
        raise SplitError("bad split %r" % (spec,))
    if method == "equal":
        return split_equal(total, body.split(","))
    pairs = _pairs(body)
    try:
        if method == "exact":
            return split_exact(total, collections.OrderedDict(
                (n, parse_amount(v)) for n, v in pairs.items()))
        if method == "percent":
            return split_percent(total, collections.OrderedDict(
                (n, Decimal(v)) for n, v in pairs.items()))
        if method == "shares":
            return split_shares(total, collections.OrderedDict(
                (n, int(v)) for n, v in pairs.items()))
    except (MoneyError, InvalidOperation, ValueError) as exc:
        if isinstance(exc, SplitError):
            raise
        raise SplitError("bad value in split %r: %s" % (spec, exc))
    raise SplitError("unknown split method %r" % (method,))
