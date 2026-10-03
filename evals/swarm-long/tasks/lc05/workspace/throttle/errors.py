"""Exception hierarchy for the ``throttle`` package."""


class ThrottleError(Exception):
    """Base class of every error raised by ``throttle``."""


class ConfigError(ThrottleError):
    """A plan configuration could not be parsed.

    ``errors`` is the list of individual problems, each a human readable
    string. ``str(exc)`` joins them with ``"; "``.
    """

    def __init__(self, errors):
        if isinstance(errors, str):
            errors = [errors]
        self.errors = list(errors)
        ThrottleError.__init__(self, "; ".join(self.errors))


class InvalidKeyError(ThrottleError, ValueError):
    """A tenant or route could not be turned into a limiter key."""


class UnknownPlanError(ThrottleError, KeyError):
    """A tenant refers to a plan that does not exist."""

    def __str__(self):
        return Exception.__str__(self)


class QuotaExceeded(ThrottleError):
    """Raised by :meth:`throttle.quota.QuotaLedger.charge` when a charge would
    take a tenant over its quota for the current billing period."""

    def __init__(self, tenant, used, limit, amount):
        self.tenant = tenant
        self.used = used
        self.limit = limit
        self.amount = amount
        ThrottleError.__init__(
            self,
            "quota exceeded for %s: %d used + %d requested > %d" % (tenant, used, amount, limit),
        )
