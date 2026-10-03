# throttle

`throttle` is the rate limiting and quota service behind our public API
gateway. For every incoming request the gateway calls
`RateLimiter.check(tenant, route)` and gets back a `Decision` that says
whether the request may proceed and which `X-RateLimit-*` / `Retry-After`
headers to send.

Layout of the `throttle` package:

| module | purpose |
| --- | --- |
| `clock.py` | injectable clocks (`ManualClock` for tests) |
| `errors.py` | exception hierarchy |
| `keys.py` | tenant / route normalisation into limiter keys |
| `plans.py` | `Rate`, `Plan`, rate string parsing |
| `config.py` | plan configuration file parser |
| `token_bucket.py`, `sliding_window.py`, `fixed_window.py` | the limiting algorithms |
| `retry.py` | rate limit response headers |
| `periods.py` | billing periods (anchor day, UTC) |
| `quota.py` | quota accounting per billing period |
| `limiter.py` | the `RateLimiter` facade and `Decision` |
| `metrics.py`, `report.py` | decision counters and the operator report |

Nothing in the package reads the system clock: every component takes a clock
object with a `now()` method (seconds since the epoch, UTC).

Run the tests with:

    python3 -m unittest discover -s tests -t .

Python 3.6+, standard library only. Open bugs and requested changes are
tracked in `ISSUES.md`.
