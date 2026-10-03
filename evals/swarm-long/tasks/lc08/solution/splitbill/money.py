"""Money helpers.

Every amount inside splitbill is an int number of cents (minor units);
conversions to and from text happen only at the edges (`parse_amount`,
`format_cents`). Splitting never creates or loses a cent: the shares of a
split always add up to the total.

There are no floats anywhere: amounts that arrive as text go through
`decimal.Decimal`, never through `float`.
"""

from decimal import Decimal, InvalidOperation


class MoneyError(ValueError):
    """Raised for a malformed amount or an impossible split."""


def parse_amount(text):
    """Cents of a decimal text amount: '12.5' -> 1250, '-3.10' -> -310.

    At most two decimals are allowed; surrounding whitespace is ignored.
    """
    text = text.strip()
    try:
        value = Decimal(text)
    except InvalidOperation:
        raise MoneyError("bad amount %r" % (text,))
    if not value.is_finite():
        raise MoneyError("bad amount %r" % (text,))
    cents = value * 100
    if cents != cents.to_integral_value():
        raise MoneyError("more than two decimals in %r" % (text,))
    return int(cents)


def format_cents(cents):
    """'12.50' for 1250, '-3.10' for -310."""
    sign = "-" if cents < 0 else ""
    whole, part = divmod(abs(cents), 100)
    return "%s%d.%02d" % (sign, whole, part)


def split_even(total, n):
    """Split `total` cents into `n` shares that add up to `total`."""
    if n <= 0:
        raise MoneyError("cannot split between %d people" % n)
    sign = -1 if total < 0 else 1
    base, rest = divmod(abs(total), n)
    shares = [base + 1 if i < rest else base for i in range(n)]
    return [sign * share for share in shares]


def split_weighted(total, weights):
    """Split `total` cents in proportion to `weights` (largest remainder).

    Every share first gets the whole cents of its exact proportion (rounded
    towards zero); the cents left over go one each to the shares with the
    largest fractional remainder, ties to the earlier share. Negative totals
    are split like the positive total, then negated. Weights are numbers
    (int, Decimal or numeric strings) and must not be negative.
    """
    weights = [Decimal(str(w)) for w in weights]
    if not weights or any(w < 0 for w in weights):
        raise MoneyError("weights must be non-negative and non-empty")
    weight_sum = sum(weights)
    if weight_sum == 0:
        raise MoneyError("weights add up to zero")
    sign = -1 if total < 0 else 1
    magnitude = abs(total)
    exact = [magnitude * w / weight_sum for w in weights]
    shares = [int(x) for x in exact]
    left = magnitude - sum(shares)
    order = sorted(range(len(weights)), key=lambda i: (-(exact[i] - shares[i]), i))
    for i in order[:left]:
        shares[i] += 1
    return [sign * s for s in shares]
