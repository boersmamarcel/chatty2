"""Rate limit response headers.

HTTP clients are told about their budget with these headers:

``X-RateLimit-Limit``
    the request limit of the binding rate;
``X-RateLimit-Remaining``
    how many more requests would be allowed right now;
``X-RateLimit-Reset``
    whole seconds until the budget is fully restored;
``Retry-After``
    only on refused requests: whole seconds the client must wait before the
    same request can succeed (RFC 9110 delay-seconds).

All header values are strings of non-negative integers.
"""

import math

HEADER_LIMIT = "X-RateLimit-Limit"
HEADER_REMAINING = "X-RateLimit-Remaining"
HEADER_RESET = "X-RateLimit-Reset"
HEADER_RETRY_AFTER = "Retry-After"


def reset_seconds(seconds):
    """Whole seconds for ``X-RateLimit-Reset``: rounded up, never negative."""
    if seconds <= 0:
        return 0
    return int(math.ceil(seconds))


def retry_after_seconds(seconds):
    """Whole seconds for ``Retry-After`` given the exact wait in seconds."""
    if seconds <= 0:
        return 0
    return int(round(seconds))


def format_headers(limit, remaining, reset_after, retry_after=None):
    """Build the header dict.

    :param limit: request limit of the binding rate (int).
    :param remaining: requests left; rendered with ``str()``.
    :param reset_after: seconds until the budget is restored.
    :param retry_after: seconds to wait, or ``None`` for an allowed request
        (then no ``Retry-After`` header is produced).
    """
    headers = {
        HEADER_LIMIT: str(int(limit)),
        HEADER_REMAINING: str(remaining),
        HEADER_RESET: str(reset_seconds(reset_after)),
    }
    if retry_after is not None:
        headers[HEADER_RETRY_AFTER] = str(retry_after_seconds(retry_after))
    return headers


def parse_retry_after(value):
    """Parse a ``Retry-After`` delay-seconds value back into an int.

    Returns ``None`` for values that are not a non-negative integer (HTTP-date
    forms are not supported).
    """
    value = (value or "").strip()
    if not value.isdigit():
        return None
    return int(value)
