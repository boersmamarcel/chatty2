"""Number helpers behind the numeric filters (``round``, ``filesizeformat``,
``numberformat``, ``percent``).

Template authors write prices and sizes the way people read them, so
rounding here is the "schoolbook" kind: halves round away from zero, on the
decimal value the number prints as (``2.675`` is two-six-seven-five, not the
binary float slightly below it).
"""

import math
from decimal import ROUND_CEILING, ROUND_FLOOR, ROUND_HALF_UP, Decimal, InvalidOperation

from .errors import FilterArgumentError

ROUND_METHODS = ("common", "ceil", "floor")

DECIMAL_PREFIXES = ("kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB")
BINARY_PREFIXES = ("KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB")


def to_decimal(value):
    """Convert an int, float, string or Decimal to :class:`Decimal`.

    Floats go through ``repr`` so that ``2.675`` becomes ``Decimal("2.675")``.
    Raises :class:`FilterArgumentError` for values that are not numbers.
    """
    if isinstance(value, Decimal):
        return value
    if isinstance(value, bool):
        return Decimal(int(value))
    if isinstance(value, int):
        return Decimal(value)
    if isinstance(value, float):
        if math.isnan(value) or math.isinf(value):
            raise FilterArgumentError("cannot format %r" % value)
        return Decimal(repr(value))
    try:
        return Decimal(str(value).strip())
    except InvalidOperation:
        raise FilterArgumentError("not a number: %r" % (value,))


def quantum(places):
    """``Decimal`` exponent for ``places`` digits after the point."""
    return Decimal(1).scaleb(-places)


def round_value(value, precision=0, method="common"):
    """Round ``value`` to ``precision`` decimal places; always returns a float.

    ``method`` is ``"common"`` (round half away from zero), ``"ceil"``
    (always up) or ``"floor"`` (always down).
    """
    if method not in ROUND_METHODS:
        raise FilterArgumentError("method must be common, ceil or floor")
    rounding = {"common": ROUND_HALF_UP, "ceil": ROUND_CEILING, "floor": ROUND_FLOOR}[method]
    number = to_decimal(value).quantize(quantum(int(precision)), rounding=rounding)
    return float(number)


def format_number(value, places=2, thousands=",", point="."):
    """Format with a fixed number of decimals and a thousands separator.

    >>> format_number(1234567.891)
    '1,234,567.89'
    >>> format_number(-0.005, 2)
    '-0.01'
    """
    number = to_decimal(value).quantize(quantum(int(places)), rounding=ROUND_HALF_UP)
    sign = "-" if number < 0 else ""
    digits = format(abs(number), "f")
    if "." in digits:
        whole, frac = digits.split(".")
    else:
        whole, frac = digits, ""
    groups = []
    while len(whole) > 3:
        groups.insert(0, whole[-3:])
        whole = whole[:-3]
    groups.insert(0, whole)
    text = thousands.join(groups)
    if frac:
        text += point + frac
    return sign + text


def percent(value, places=0):
    """``0.256`` -> ``"26%"`` (``places`` decimals, half-up)."""
    number = (to_decimal(value) * 100).quantize(quantum(int(places)), rounding=ROUND_HALF_UP)
    return "%s%%" % format(number, "f")


def filesizeformat(value, binary=False):
    """Human readable file size: ``"1 Byte"``, ``"999 Bytes"``, ``"1.0 kB"``,
    ``"2.5 MB"``; with ``binary=True`` powers of 1024 and ``KiB``/``MiB``...
    """
    size = to_decimal(value)
    base = 1024 if binary else 1000
    prefixes = BINARY_PREFIXES if binary else DECIMAL_PREFIXES
    if size == 1:
        return "1 Byte"
    if size < base:
        return "%d Bytes" % int(size)
    for i, prefix in enumerate(prefixes):
        unit = base ** (i + 2)
        if size < unit or i == len(prefixes) - 1:
            scaled = (size * base / unit).quantize(Decimal("0.1"), rounding=ROUND_HALF_UP)
            return "%s %s" % (scaled, prefix)
