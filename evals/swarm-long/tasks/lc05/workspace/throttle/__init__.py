"""throttle: API rate limiting and quota service.

Public entry points:

* :class:`throttle.limiter.RateLimiter` - the facade used by the API gateway;
* :func:`throttle.config.parse_config` - plan configuration parsing;
* :mod:`throttle.token_bucket`, :mod:`throttle.sliding_window`,
  :mod:`throttle.fixed_window` - the limiting algorithms;
* :class:`throttle.quota.QuotaLedger` - quota accounting per billing period;
* :func:`throttle.report.render` - the operator report.

Every component takes an injectable clock (:mod:`throttle.clock`).
"""

__version__ = "0.4.2"
