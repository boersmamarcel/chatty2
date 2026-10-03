"""Exchange rates and conversion.

Rates are quoted against the base currency EUR: a rates file line

    USD 1.0850

means 1 EUR = 1.0850 USD. The base currency itself always has rate 1 and
does not need a line. Currency codes are three letters, case-insensitive.
"""

from decimal import ROUND_HALF_UP, Decimal, InvalidOperation

BASE = "EUR"


class CurrencyError(ValueError):
    """Raised for an unknown currency or a malformed rates file."""


def parse_rates(text):
    """Dict code -> Decimal rate (units per 1 EUR), including EUR itself."""
    rates = {BASE: Decimal(1)}
    for line_no, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if len(fields) != 2 or len(fields[0]) != 3 or not fields[0].isalpha():
            raise CurrencyError("rates line %d: expected '<CODE> <rate>'" % line_no)
        try:
            rate = Decimal(fields[1])
        except InvalidOperation:
            raise CurrencyError("rates line %d: bad rate %r" % (line_no, fields[1]))
        if rate <= 0:
            raise CurrencyError("rates line %d: rate must be positive" % line_no)
        rates[fields[0].upper()] = rate
    return rates


def normalize_code(code):
    """Upper-case, stripped currency code; CurrencyError unless 3 letters."""
    code = (code or "").strip().upper()
    if len(code) != 3 or not code.isalpha():
        raise CurrencyError("bad currency code %r" % (code,))
    return code


def known_currencies(rates):
    """Sorted codes that `rates` can convert between (always includes EUR)."""
    return sorted(set(rates) | {BASE})


def convert(cents, src, dst, rates):
    """Convert `cents` of currency `src` into cents of currency `dst`."""
    src = normalize_code(src)
    dst = normalize_code(dst)
    for code in (src, dst):
        if code != BASE and code not in rates:
            raise CurrencyError("unknown currency %r" % (code,))
    if src == dst:
        return cents
    src_rate = Decimal(1) if src == BASE else rates[src]
    dst_rate = Decimal(1) if dst == BASE else rates[dst]
    amount = Decimal(cents) / src_rate * dst_rate
    return int(amount.quantize(Decimal(1), rounding=ROUND_HALF_UP))
