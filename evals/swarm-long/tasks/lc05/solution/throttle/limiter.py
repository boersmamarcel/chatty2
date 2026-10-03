"""The rate limiter facade.

:class:`RateLimiter` ties everything together: it normalises the request key,
looks up the tenant's plan, enforces every rate of the plan with the plan's
algorithm (one limiter instance per key and rate), charges the monthly quota,
records metrics and returns a :class:`Decision` that can render the HTTP
headers for the response.

A request is allowed only when *every* rate of the plan allows it and the
quota (if any) is not exhausted. Nothing is consumed for a refused request.
"""

import math

from .errors import UnknownPlanError
from .fixed_window import FixedWindowCounter
from .keys import normalise_key, normalise_tenant
from .metrics import Metrics
from .quota import QuotaLedger
from .retry import format_headers
from .sliding_window import SlidingWindowLog
from .token_bucket import TokenBucket

REASON_RATE = "rate"
REASON_QUOTA = "quota"


class Decision(object):
    """Outcome of :meth:`RateLimiter.check`.

    :ivar allowed: True when the request may proceed.
    :ivar key: the normalised limiter key.
    :ivar limit: request limit of the binding rate (or the quota).
    :ivar remaining: requests left under the binding rate (may be fractional
        for token buckets).
    :ivar reset_after: seconds until the binding budget is fully restored.
    :ivar retry_after: seconds to wait before retrying; ``None`` when allowed.
    :ivar reason: ``None`` when allowed, else ``"rate"`` or ``"quota"``.
    """

    def __init__(self, allowed, key, limit, remaining, reset_after, retry_after=None, reason=None):
        self.allowed = allowed
        self.key = key
        self.limit = limit
        self.remaining = remaining
        self.reset_after = reset_after
        self.retry_after = retry_after
        self.reason = reason

    def headers(self):
        """HTTP response headers for this decision (see :mod:`throttle.retry`)."""
        retry_after = None if self.allowed else self.retry_after
        remaining = max(0, int(math.floor(self.remaining)))
        return format_headers(self.limit, remaining, self.reset_after, retry_after)

    def __repr__(self):
        return "Decision(allowed=%r, key=%r, limit=%r, remaining=%r, retry_after=%r, reason=%r)" % (
            self.allowed, self.key, self.limit, self.remaining, self.retry_after, self.reason)


def _limit_of(limiter):
    """Request limit enforced by a limiter instance."""
    if isinstance(limiter, TokenBucket):
        return limiter.capacity
    return limiter.limit


def build_limiter(plan, rate, clock):
    """Create the limiter instance enforcing ``rate`` under ``plan``."""
    if plan.algorithm == "token_bucket":
        return TokenBucket.for_rate(rate, clock, burst=plan.burst)
    if plan.algorithm == "sliding_window":
        return SlidingWindowLog.for_rate(rate, clock)
    if plan.algorithm == "fixed_window":
        return FixedWindowCounter.for_rate(rate, clock)
    raise ValueError("unknown algorithm %r" % (plan.algorithm,))


class RateLimiter(object):
    """Per-tenant, per-route rate limiting.

    :param plans: dict plan name -> :class:`~throttle.plans.Plan`.
    :param clock: object with ``now()``.
    :param tenants: dict tenant -> plan name (tenant names are normalised).
    :param default_plan: plan name for tenants not in ``tenants``.
    :param metrics: a :class:`~throttle.metrics.Metrics` to record into
        (a fresh one by default).
    """

    def __init__(self, plans, clock, tenants=None, default_plan=None, metrics=None):
        self.plans = dict(plans)
        self.clock = clock
        self.default_plan = default_plan
        self.metrics = metrics if metrics is not None else Metrics()
        self.ledger = QuotaLedger(clock)
        self._tenants = {}
        self._limiters = {}
        for tenant, plan_name in (tenants or {}).items():
            self.assign(tenant, plan_name)

    @classmethod
    def from_config(cls, config, clock, default_plan=None, metrics=None):
        """Limiter for a parsed :class:`~throttle.config.Config`."""
        return cls(config.plans, clock, tenants=config.tenants,
                   default_plan=default_plan, metrics=metrics)

    def assign(self, tenant, plan_name):
        """Put ``tenant`` on plan ``plan_name``. Existing budgets are dropped."""
        if plan_name not in self.plans:
            raise UnknownPlanError("unknown plan '%s'" % plan_name)
        tenant = normalise_tenant(tenant)
        self._tenants[tenant] = plan_name
        prefix = tenant + ":"
        for key in [k for k in self._limiters if k[0].startswith(prefix)]:
            del self._limiters[key]

    def plan_for(self, tenant):
        """The :class:`~throttle.plans.Plan` of ``tenant``."""
        name = self._tenants.get(normalise_tenant(tenant), self.default_plan)
        if name is None or name not in self.plans:
            raise UnknownPlanError("no plan for tenant '%s'" % tenant)
        return self.plans[name]

    def _limiters_for(self, key, plan):
        found = []
        for index, rate in enumerate(plan.rates):
            slot = (key, plan.name, index)
            limiter = self._limiters.get(slot)
            if limiter is None:
                limiter = self._limiters[slot] = build_limiter(plan, rate, self.clock)
            found.append(limiter)
        return found

    @staticmethod
    def _binding(limiters):
        """The limiter with the fewest remaining requests (first on ties)."""
        best = None
        best_remaining = None
        for limiter in limiters:
            remaining = limiter.remaining()
            if best is None or remaining < best_remaining:
                best, best_remaining = limiter, remaining
        return best, best_remaining

    def check(self, tenant, route, cost=1):
        """Decide whether ``tenant`` may call ``route`` now, consuming budget
        when allowed. Returns a :class:`Decision`."""
        key = normalise_key(tenant, route)
        tenant_name = normalise_tenant(tenant)
        plan = self.plan_for(tenant)
        limiters = self._limiters_for(key, plan)

        if plan.quota is not None and self.ledger.would_exceed(
                tenant_name, cost, plan.quota, plan.anchor_day):
            wait = self.ledger.seconds_until_reset(tenant_name, plan.anchor_day)
            decision = Decision(False, key, plan.quota, 0, wait, wait, REASON_QUOTA)
            self.metrics.record(tenant_name, False, wait)
            return decision

        blocking = [lim for lim in limiters if not lim.can_acquire(cost)]
        if blocking:
            wait = max(lim.retry_after(cost) for lim in blocking)
            binding, remaining = self._binding(limiters)
            decision = Decision(False, key, _limit_of(binding), remaining,
                                binding.reset_after(), wait, REASON_RATE)
            self.metrics.record(tenant_name, False, wait)
            return decision

        for limiter in limiters:
            limiter.try_acquire(cost)
        if plan.quota is not None:
            self.ledger.charge(tenant_name, cost, plan.quota, plan.anchor_day)
        if limiters:
            binding, remaining = self._binding(limiters)
            decision = Decision(True, key, _limit_of(binding), remaining, binding.reset_after())
        else:
            decision = Decision(True, key, 0, 0, 0.0)
        self.metrics.record(tenant_name, True)
        return decision

    def remaining(self, tenant, route):
        """Requests ``tenant`` could still make on ``route`` right now
        (rounded down), without consuming anything."""
        key = normalise_key(tenant, route)
        limiters = self._limiters_for(key, self.plan_for(tenant))
        if not limiters:
            return 0
        return int(math.floor(self._binding(limiters)[1]))
