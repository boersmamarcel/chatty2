# Open issues

Each issue below is independent. Unless stated otherwise, keep every existing
name, signature and behaviour that the issue does not mention.

## Issue 1: token bucket hands out more than its capacity after idle time

Customers on the `token_bucket` algorithm report bursts far larger than their
plan's burst size after a quiet period: a bucket with capacity 5 that sat idle
for an hour let 1,800 requests through in one go.

Acceptance (`throttle/token_bucket.py`, class `TokenBucket`):

1. The number of tokens never exceeds `capacity`: refilling stops at a full
   bucket. `available()` after any amount of idle time is at most `capacity`
   and a full bucket then allows exactly `capacity` requests of cost 1 in a
   row (the next one is refused) when the clock does not move.
2. Partial refills still work as before: a bucket of capacity 4 refilling at
   0.5 tokens/s that was emptied at t=0 has 1.5 tokens at t=3.
3. A cost larger than the capacity can never succeed, so asking for it is a
   programming error: `try_acquire(cost)`, `can_acquire(cost)` and
   `time_until(cost)` raise `ValueError` with the message
   `cost <cost> exceeds bucket capacity <capacity>` (values formatted with
   `%s`, e.g. `cost 6 exceeds bucket capacity 5`) when `cost > capacity`.
   A cost equal to the capacity is valid. The existing
   `ValueError("cost must be positive")` for `cost <= 0` stays.

## Issue 2: wrong Retry-After / X-RateLimit-Remaining header values

Clients that honour our headers retry too early and get refused again, and
some HTTP libraries choke on `X-RateLimit-Remaining: 2.5`.

Acceptance (headers produced by `Decision.headers()` in
`throttle/limiter.py`, built with the helpers in `throttle/retry.py`):

1. `retry.retry_after_seconds(seconds)` converts an exact wait into whole
   seconds by rounding **up**, and any positive wait gives at least 1:
   `0.25 -> 1`, `1.5 -> 2`, `2.5 -> 3`, `3.0 -> 3`, `0.0001 -> 1`.
   A wait `<= 0` gives `0`. The return value is an `int`.
2. `Retry-After` (only present on refused decisions, as today) uses that
   conversion.
3. `X-RateLimit-Remaining` is a whole number: the remaining budget rounded
   **down**, and never below `0`. A token bucket with 2.875 tokens left
   reports `"2"`; 0.5 tokens left reports `"0"`; 3.0 reports `"3"`.
4. `X-RateLimit-Limit` and `X-RateLimit-Reset` are unchanged (the reset value
   is already rounded up). All header values are strings.

Example: plan with one `Rate(4, 8)` on `token_bucket` (capacity 4, refill
0.5 tokens/s). The first request at t=1000 gives
`{"X-RateLimit-Limit": "4", "X-RateLimit-Remaining": "3", "X-RateLimit-Reset": "2"}`.

## Issue 3: sliding window counts requests that are a full window old

The `sliding_window` algorithm is documented as using the half-open window
`(t - window, t]`, but a request made exactly `window` seconds ago still
counts against the client. A second problem: clients that keep hammering
while refused never get through again, because refused attempts are logged
as if they had been allowed.

Acceptance (`throttle/sliding_window.py`, class `SlidingWindowLog`):

1. An entry recorded at time `r` stops counting at time `r + window`
   exactly (it still counts at any time before that).
2. A refused `try_acquire` records nothing: `count()` is unchanged by a
   refused call, whatever its `cost`.
3. `retry_after(cost)` keeps its documented meaning and returns the exact
   wait (e.g. limit 2, window 10: requests at t=0 and t=4, a refused attempt
   at t=6 -> `retry_after()` is `4.0`; after any number of further refused
   attempts at t=7 it is `3.0`).

## Issue 4: rate strings are case sensitive and the "per" form is rejected; config errors stop at the first problem

Operators write rates like `100/Min` or `30 per 5 minutes` and get
`ConfigError`s. When they fix that line they get the next error, one at a
time.

Acceptance:

1. `plans.parse_rate` ignores case in the unit (`"100/Min"`, `"10/S"`,
   `"2/HOUR"` are valid) and accepts, besides `<count>/<unit>` and
   `<count>/<multiplier><unit>`, the form `<count> per <unit>` and
   `<count> per <multiplier> <unit>` (the word `per` in any case,
   surrounded by whitespace), e.g. `"30 per minute" -> Rate(30, 60)`,
   `"30 per 5 minutes" -> Rate(30, 300)`, `"7 PER day" -> Rate(7, 86400)`.
   Surrounding whitespace is ignored as before. The error messages stay
   exactly as they are; in `unknown unit '<unit>'` the unit is shown
   lowercased.
2. `config.parse_config` reports **all** problems of a file in one
   `ConfigError`: `exc.errors` is the list of every message, each prefixed
   with `line N: ` (N is 1-based), ordered by line number; problems on the
   same line keep the order in which they were found. `plan '<name>' has no
   rate` is reported on the line of the `[plan <name>]` header, and a `rate`
   line that failed to parse does not count as a rate. Parsing continues
   after every problem; the existing messages are unchanged.
3. A file without problems parses exactly as before.

Example: for the file

    [plan free]
    rate = 10/Second
    rate = 5 per hours
    burst = lots
    [plan empty]
    rate = 3/fortnight
    [tenants]
    acme = gold

`exc.errors` is `["line 4: burst must be an integer", "line 5: plan 'empty' has no rate",
"line 6: unknown unit 'fortnight'", "line 8: tenant 'acme' refers to unknown plan 'gold'"]`.

## Issue 5: billing periods with anchor day 29-31 are wrong around short months

Quota usage of tenants whose billing anchor day is the 29th, 30th or 31st is
reset on the wrong day (e.g. on March 3 instead of February 28), and usage at
the exact moment a new period starts is still counted in the old period.

Acceptance (`throttle/periods.py`; the documented rules in the module
docstring are correct):

1. `anchor_date(year, month, anchor_day)` returns 00:00 UTC on
   `min(anchor_day, last day of that month)`.
2. A period is the half-open interval `[start, end)`:
   `BillingPeriod.contains(end)` is `False` and `contains(start)` is `True`.
3. `billing_period(anchor_day, ts)` returns the period containing `ts`; a
   timestamp exactly at a period start belongs to the period that starts
   there. Examples (anchor 31): 2027-02-28 00:00 UTC lies in
   `[2027-02-28, 2027-03-31)`, 2027-02-27 23:59:59 in
   `[2027-01-31, 2027-02-28)`; in the leap year 2028 anchor 30 gives
   `[2028-01-30, 2028-02-29)` and `[2028-02-29, 2028-03-30)`. Year
   boundaries work too: anchor 31, 2027-01-15 lies in
   `[2026-12-31, 2027-01-31)`.

## Issue 6: equivalent routes get separate rate limit budgets

`/v1/orders/`, `/v1/orders?page=2` and `//v1/orders` are all the same
endpoint, but each gets its own budget, so clients can multiply their limit.
Routes with UUID identifiers are not collapsed either.

Acceptance (`throttle/keys.py`, `normalise_route` and therefore
`normalise_key`):

1. Everything from the first `?` or `#` on is dropped (query string and
   fragment).
2. Empty path segments are removed: repeated slashes collapse into one and a
   trailing slash is dropped. The result always starts with `/`; a route
   that is empty, only whitespace, only slashes, only a query string or
   `None` becomes `/`.
3. Besides purely numeric segments, every segment that is a UUID
   (8-4-4-4-12 hexadecimal digits separated by `-`, any letter case) is
   replaced by `{id}`. Other segments (e.g. `abc123`, or a UUID without
   dashes) are kept.
4. Surrounding whitespace is stripped and the route is lowercased as before;
   tenant handling and the key format `<tenant>:<route>` do not change.

Examples: `normalise_route("/v1/Orders/?page=2") == "/v1/orders"`,
`normalise_route("v1//users/42/") == "/v1/users/{id}"`,
`normalise_key("Acme", "/v1/files/3F2504E0-4F89-11D3-9A0C-0305E82C3301#x") == "acme:/v1/files/{id}"`.

## Issue 7: report rounding and row order

The operator report shows `6.2` for a tenant with 1 of 16 requests denied
(6.25 %), and tenants with the same number of denials come out in reverse
alphabetical order.

Acceptance:

1. `metrics.deny_percent(allowed, denied)` returns a `decimal.Decimal`
   rounded to one decimal place with ROUND_HALF_UP (`1` of `16` ->
   `Decimal("6.3")`, `1` of `80` -> `Decimal("1.3")`, `2` of `3` ->
   `Decimal("66.7")`), and `Decimal("0.0")` when there were no decisions.
   `Metrics.deny_rate(tenant)` returns the same value.
2. The report (`report.render`) lists tenant rows by number of denied
   decisions, highest first; ties are broken by the deny percentage as
   shown in the report (rounded as above), highest first; remaining ties by
   tenant name in ascending order. `top` keeps the first `top` rows of that
   order. The `TOTAL` row and the table format are unchanged.
