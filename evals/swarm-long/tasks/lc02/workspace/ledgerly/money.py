"""Decimal money helpers.

All amounts in ledgerly are :class:`decimal.Decimal`.  Floats are never used
for arithmetic: they are accepted at the edges only and converted through
their ``repr`` so that ``0.1`` becomes ``Decimal('0.1')`` and not the binary
approximation.

Rounding is always ``ROUND_HALF_UP`` (commercial rounding: 0.125 -> 0.13) to
the number of minor units of the currency (2 for most currencies, 0 for JPY,
3 for BHD).
"""

from decimal import Decimal, InvalidOperation, ROUND_HALF_UP

ZERO = Decimal("0")
DEFAULT_DIGITS = 2

#: Currencies whose minor unit is not 1/100.
CURRENCY_DIGITS = {
    "JPY": 0,
    "KRW": 0,
    "ISK": 0,
    "BHD": 3,
    "KWD": 3,
    "OMR": 3,
}


def normalize_currency(code):
    """Return ``code`` stripped and upper-cased (``" usd "`` -> ``"USD"``).

    ``None`` is passed through.  Anything that is not three letters after
    normalisation raises ``ValueError``.
    """
    if code is None:
        return None
    text = str(code).strip().upper()
    if len(text) != 3 or not text.isalpha():
        raise ValueError("invalid currency code %r" % (code,))
    return text


def minor_units(currency=None):
    """Number of decimal places used by ``currency`` (default 2)."""
    if currency is None:
        return DEFAULT_DIGITS
    return CURRENCY_DIGITS.get(normalize_currency(currency), DEFAULT_DIGITS)


def quantum(currency=None):
    """The smallest amount of ``currency`` as a Decimal (``0.01``, ``1``...)."""
    return Decimal(1).scaleb(-minor_units(currency))


def to_decimal(value):
    """Convert ``value`` (Decimal, int, str or float) to a Decimal.

    Strings are stripped first.  Booleans are rejected because ``True`` is an
    int in Python and would silently become ``1``.
    """
    if isinstance(value, bool):
        raise TypeError("booleans are not amounts")
    if isinstance(value, Decimal):
        return value
    if isinstance(value, int):
        return Decimal(value)
    if isinstance(value, float):
        return Decimal(repr(value))
    if isinstance(value, str):
        try:
            return Decimal(value.strip())
        except InvalidOperation:
            raise ValueError("invalid amount %r" % (value,))
    raise TypeError("cannot convert %r to an amount" % (value,))


def quantize(amount, currency=None):
    """Round ``amount`` half-up to the minor units of ``currency``."""
    return to_decimal(amount).quantize(quantum(currency), rounding=ROUND_HALF_UP)


def sum_amounts(values):
    """Sum an iterable of amounts, starting from ``Decimal('0')``."""
    total = ZERO
    for value in values:
        total += to_decimal(value)
    return total


def is_zero(amount, currency=None):
    """True when ``amount`` rounds to zero in ``currency``."""
    return quantize(amount, currency) == ZERO


def allocate(amount, weights, currency=None):
    """Split ``amount`` proportionally to ``weights`` without losing cents.

    Uses the largest-remainder method: every share is first rounded down to
    the currency quantum, then the leftover quanta go to the shares with the
    largest remainders (ties: earlier position first).  The returned shares
    always sum to ``quantize(amount)``.
    """
    total = quantize(amount, currency)
    weights = [to_decimal(w) for w in weights]
    if not weights:
        raise ValueError("allocate needs at least one weight")
    weight_sum = sum_amounts(weights)
    if weight_sum <= 0:
        raise ValueError("weights must sum to a positive number")
    step = quantum(currency)
    raw = [total * w / weight_sum for w in weights]
    shares = [(r / step).to_integral_value(rounding="ROUND_FLOOR") * step for r in raw]
    leftover = int((total - sum_amounts(shares)) / step)
    order = sorted(range(len(raw)), key=lambda i: (-(raw[i] - shares[i]), i))
    for i in order[:leftover]:
        shares[i] += step
    return shares
