"""Money helpers built on :class:`decimal.Decimal`.

All monetary amounts in stockroom are Decimals. Floats are only accepted at
the edges (user input) and are converted through ``str`` so that ``0.1``
becomes ``Decimal("0.1")`` and not the binary approximation.
"""

from decimal import Decimal, InvalidOperation, ROUND_HALF_UP

CENT = Decimal("0.01")
ZERO = Decimal("0")


class MoneyError(ValueError):
    """Raised when a value cannot be interpreted as an amount."""


def to_decimal(value):
    """Convert ``value`` (Decimal, int, float or str) to a Decimal.

    Strings may contain surrounding whitespace, a leading currency sign
    (``$``, ``EUR``) and thousands separators (``1,250.00``).

    >>> to_decimal(" $1,250.5 ")
    Decimal('1250.5')
    """
    if isinstance(value, Decimal):
        return value
    if isinstance(value, bool):
        raise MoneyError("booleans are not amounts")
    if isinstance(value, int):
        return Decimal(value)
    if isinstance(value, float):
        return Decimal(str(value))
    if not isinstance(value, str):
        raise MoneyError("cannot convert %r to an amount" % (value,))
    text = value.strip()
    for sign in ("EUR", "USD", "$", "€"):
        if text.startswith(sign):
            text = text[len(sign):].strip()
    text = text.replace(",", "")
    if not text:
        raise MoneyError("empty amount")
    try:
        return Decimal(text)
    except InvalidOperation:
        raise MoneyError("not an amount: %r" % (value,))


def round_money(value, places=2):
    """Round half-up to ``places`` decimals (commercial rounding).

    >>> round_money(Decimal("2.675"))
    Decimal('2.68')
    """
    quantum = Decimal(1).scaleb(-places)
    return to_decimal(value).quantize(quantum, rounding=ROUND_HALF_UP)


def allocate_amount(total, weights):
    """Split ``total`` over ``weights`` in cents so the parts sum to ``total``.

    The remainder cents go to the parts with the largest fractional share,
    earlier parts first on ties. Used to spread freight cost over receipt
    lines.

    >>> allocate_amount(Decimal("10.00"), [1, 1, 1])
    [Decimal('3.34'), Decimal('3.33'), Decimal('3.33')]
    """
    total = round_money(total)
    weights = [to_decimal(w) for w in weights]
    if not weights:
        return []
    weight_sum = sum(weights)
    if weight_sum <= 0:
        raise MoneyError("weights must sum to a positive number")
    cents = int(total / CENT)
    raw = [Decimal(cents) * w / weight_sum for w in weights]
    floors = [int(r) for r in raw]
    remainder = cents - sum(floors)
    order = sorted(range(len(raw)), key=lambda i: (-(raw[i] - floors[i]), i))
    for i in order[:remainder]:
        floors[i] += 1
    return [Decimal(c) * CENT for c in floors]


def format_amount(value, places=2):
    """Format an amount for reports: ``1234.5`` -> ``"1234.50"``.

    Rounds half-up to ``places`` decimals first.
    """
    rounded = round_money(value, places)
    return "{0:.{1}f}".format(rounded, places)


def sum_amounts(values):
    """Sum an iterable of amounts exactly (no float drift)."""
    total = ZERO
    for v in values:
        total += to_decimal(v)
    return total
