"""VAT codes and tax arithmetic.

A tax code has a rate expressed as a fraction (``Decimal("0.21")`` for 21 %)
and the liability account the tax is credited to.  Exempt codes carry no tax
at all; zero-rated codes have rate 0.  Tax amounts are rounded half-up to the
minor units of the currency.
"""

from decimal import Decimal

from .errors import TaxCodeError
from .money import ZERO, minor_units, quantize, to_decimal


class TaxCode(object):
    """One VAT code."""

    __slots__ = ("code", "rate", "account", "description", "exempt")

    def __init__(self, code, rate, account, description="", exempt=False):
        self.code = str(code).strip().upper()
        self.rate = to_decimal(rate)
        if self.rate < 0 or self.rate >= 1:
            raise TaxCodeError("tax rate of %s must be in [0, 1)" % self.code)
        if exempt and self.rate != 0:
            raise TaxCodeError("exempt code %s cannot have a rate" % self.code)
        self.account = account
        self.description = description
        self.exempt = exempt

    @property
    def percent(self):
        """The rate as a percentage string, e.g. ``"21%"``."""
        return "%s%%" % (self.rate * 100).normalize()

    def __repr__(self):
        return "TaxCode(%r, %s)" % (self.code, self.rate)


class TaxTable(object):
    """The tax codes in use, keyed by code."""

    def __init__(self, codes=()):
        self._codes = {}
        for code in codes:
            self.add(code)

    def add(self, tax_code):
        if tax_code.code in self._codes:
            raise TaxCodeError("duplicate tax code %s" % tax_code.code)
        self._codes[tax_code.code] = tax_code
        return tax_code

    def get(self, code):
        """The :class:`TaxCode` for ``code`` (case-insensitive)."""
        key = str(code).strip().upper()
        try:
            return self._codes[key]
        except KeyError:
            raise TaxCodeError("unknown tax code %s" % key)

    def codes(self):
        return sorted(self._codes)

    def __contains__(self, code):
        return str(code).strip().upper() in self._codes


def default_tax_table(output_account="2100"):
    """Standard (21 %), reduced (9 %), zero-rated and exempt codes."""
    return TaxTable([
        TaxCode("S", "0.21", output_account, "Standard rate"),
        TaxCode("R", "0.09", output_account, "Reduced rate"),
        TaxCode("Z", "0", output_account, "Zero rated"),
        TaxCode("E", "0", None, "Exempt", exempt=True),
    ])


def compute_tax(net, rate, currency=None):
    """Tax on a net amount: ``net * rate`` rounded to the currency."""
    return quantize(to_decimal(net) * to_decimal(rate), currency)


def gross_up(net, rate, currency=None):
    """Gross amount for a net amount: ``net + compute_tax(net, rate)``."""
    net = quantize(net, currency)
    return net + compute_tax(net, rate, currency)


def split_gross(gross, rate, currency=None):
    """Split a tax-inclusive amount into ``(net, tax)``."""
    gross = quantize(gross, currency)
    rate = to_decimal(rate)
    tax = quantize(gross * rate / (1 + rate), currency)
    net = gross - tax
    return net, tax


def effective_rate(net, tax):
    """Tax as a fraction of net, to four decimals (``ZERO`` for zero net)."""
    net = to_decimal(net)
    if net == 0:
        return ZERO
    return (to_decimal(tax) / net).quantize(Decimal("0.0001"))
