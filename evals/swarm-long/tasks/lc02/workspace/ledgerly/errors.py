"""Exception hierarchy of ledgerly.

Every error raised on purpose by the package derives from :class:`LedgerError`
so callers can catch the whole family at once.
"""


class LedgerError(Exception):
    """Base class of every ledgerly error."""


class ValidationError(LedgerError):
    """A journal entry or a line is malformed (missing lines, bad amounts)."""


class UnknownAccountError(LedgerError):
    """An account code is not in the chart of accounts."""


class UnbalancedEntryError(ValidationError):
    """Debits and credits of an entry differ."""


class PeriodLockedError(LedgerError):
    """A posting or a close targets a period that is already closed."""


class RateNotFound(LedgerError):
    """No exchange rate is available for a currency on a given day."""


class TaxCodeError(LedgerError):
    """An unknown or invalid VAT/tax code was used."""


class ImportErrors(LedgerError):
    """One or more rows of an imported file are invalid.

    ``errors`` holds every message, in the order they were found.
    """

    def __init__(self, errors):
        self.errors = list(errors)
        super(ImportErrors, self).__init__("%d import error(s): %s" % (
            len(self.errors), "; ".join(self.errors)))
