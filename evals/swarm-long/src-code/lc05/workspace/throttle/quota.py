"""Quota accounting per billing period.

A :class:`QuotaLedger` counts how many requests (or other units) each tenant
used in its current billing period. When the clock enters a new billing period
the tenant's usage starts again from zero; the totals of finished periods are
kept in :meth:`QuotaLedger.history` for invoicing.
"""

from .errors import QuotaExceeded
from .periods import billing_period, from_timestamp


class _Account(object):
    __slots__ = ("period", "used")

    def __init__(self, period):
        self.period = period
        self.used = 0


class QuotaLedger(object):
    """Per-tenant usage counters keyed by billing period.

    :param clock: object with ``now()``.
    """

    def __init__(self, clock):
        self.clock = clock
        self._accounts = {}
        self._history = {}

    def _account(self, tenant, anchor_day):
        """The tenant's account for the current period, rolling over if needed."""
        period = billing_period(anchor_day, self.clock.now())
        account = self._accounts.get(tenant)
        if account is None or account.period != period:
            if account is not None and account.used:
                self._history.setdefault(tenant, []).append((account.period, account.used))
            account = _Account(period)
            self._accounts[tenant] = account
        return account

    def usage(self, tenant, anchor_day=1):
        """Units used by ``tenant`` in the current billing period."""
        return self._account(tenant, anchor_day).used

    def remaining(self, tenant, limit, anchor_day=1):
        """Units ``tenant`` may still use this period under ``limit``."""
        return max(0, limit - self.usage(tenant, anchor_day))

    def would_exceed(self, tenant, amount, limit, anchor_day=1):
        """True when charging ``amount`` would take usage above ``limit``."""
        return self.usage(tenant, anchor_day) + amount > limit

    def charge(self, tenant, amount, limit, anchor_day=1):
        """Add ``amount`` units to the tenant's usage.

        Raises :class:`QuotaExceeded` (and records nothing) when the new usage
        would be above ``limit``. Returns the new usage.
        """
        if amount < 0:
            raise ValueError("amount must not be negative")
        account = self._account(tenant, anchor_day)
        if account.used + amount > limit:
            raise QuotaExceeded(tenant, account.used, limit, amount)
        account.used += amount
        return account.used

    def seconds_until_reset(self, tenant, anchor_day=1):
        """Seconds until the tenant's current billing period ends."""
        account = self._account(tenant, anchor_day)
        return account.period.seconds_left(from_timestamp(self.clock.now()))

    def history(self, tenant):
        """Finished periods of ``tenant`` as ``[(BillingPeriod, used), ...]``,
        oldest first. Periods without usage are not listed."""
        return list(self._history.get(tenant, []))

    def tenants(self):
        """Sorted list of tenants with an account."""
        return sorted(self._accounts)
