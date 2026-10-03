"""Date and time helpers shared by the rota package.

The package works with *naive* datetimes (local wall-clock time of the site)
or with datetimes carrying a fixed UTC offset (``datetime.timezone``).  It never
consults the tz database: daylight-saving transitions are the caller's
business.  Mixing naive and aware values in one computation is an error.
"""

import re
from datetime import date, datetime, time, timedelta, timezone

from .errors import ParseError

WEEKDAY_CODES = ("MO", "TU", "WE", "TH", "FR", "SA", "SU")

_DATE_RE = re.compile(r"^(\d{4})-(\d{2})-(\d{2})$")
_TIME_RE = re.compile(r"^(\d{1,2}):(\d{2})(?::(\d{2}))?$")
_DT_RE = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2}))?"
    r"(Z|[+-]\d{2}:?\d{2})?$")
_OFFSET_RE = re.compile(r"^([+-])(\d{2}):?(\d{2})$")
_DURATION_RE = re.compile(r"^(?:(\d+)\s*h)?\s*(?:(\d+)\s*m(?:in)?)?$")


def parse_date(text):
    """Parse ``YYYY-MM-DD`` into a :class:`datetime.date`."""
    m = _DATE_RE.match(text.strip())
    if not m:
        raise ParseError("invalid date: %r" % text)
    try:
        return date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
    except ValueError as exc:
        raise ParseError("invalid date: %r (%s)" % (text, exc))


def parse_time(text):
    """Parse ``HH:MM`` or ``HH:MM:SS`` into a :class:`datetime.time`.

    ``24:00`` is not accepted; use ``00:00`` of the next day instead.
    """
    m = _TIME_RE.match(text.strip())
    if not m:
        raise ParseError("invalid time: %r" % text)
    hour, minute = int(m.group(1)), int(m.group(2))
    second = int(m.group(3) or 0)
    if hour > 23 or minute > 59 or second > 59:
        raise ParseError("invalid time: %r" % text)
    return time(hour, minute, second)


def parse_offset(text):
    """Parse ``Z``, ``+HH:MM``, ``-HHMM`` into a fixed :class:`timezone`."""
    text = text.strip()
    if text in ("Z", "z"):
        return timezone.utc
    m = _OFFSET_RE.match(text)
    if not m:
        raise ParseError("invalid UTC offset: %r" % text)
    hours, minutes = int(m.group(2)), int(m.group(3))
    if hours > 23 or minutes > 59:
        raise ParseError("invalid UTC offset: %r" % text)
    delta = timedelta(hours=hours, minutes=minutes)
    if m.group(1) == "-":
        delta = -delta
    return timezone(delta)


def parse_datetime(text):
    """Parse an ISO-like timestamp.

    Accepted forms: ``2026-03-01T09:00``, ``2026-03-01 09:00:30``, optionally
    followed by ``Z`` or a fixed offset such as ``+02:00``.  Without an offset
    the result is naive.
    """
    m = _DT_RE.match(text.strip())
    if not m:
        raise ParseError("invalid datetime: %r" % text)
    parts = [int(g) for g in m.groups()[:5]]
    second = int(m.group(6) or 0)
    tzinfo = parse_offset(m.group(7)) if m.group(7) else None
    try:
        return datetime(parts[0], parts[1], parts[2], parts[3], parts[4], second,
                        tzinfo=tzinfo)
    except ValueError as exc:
        raise ParseError("invalid datetime: %r (%s)" % (text, exc))


def parse_duration(text):
    """Parse a duration into whole minutes.

    ``"90"`` (plain minutes), ``"8h"``, ``"45m"``, ``"7h30m"`` and
    ``"7 h 30 min"`` are accepted.
    """
    text = text.strip().lower()
    if text.isdigit():
        return int(text)
    m = _DURATION_RE.match(text)
    if not text or not m or (m.group(1) is None and m.group(2) is None):
        raise ParseError("invalid duration: %r" % text)
    return int(m.group(1) or 0) * 60 + int(m.group(2) or 0)


def weekday_code(day):
    """Two-letter iCalendar code (``MO`` .. ``SU``) of a date or datetime."""
    return WEEKDAY_CODES[day.weekday()]


def weekday_index(code):
    """Index 0 (Monday) .. 6 (Sunday) of a two-letter weekday code."""
    code = code.strip().upper()
    if code not in WEEKDAY_CODES:
        raise ParseError("invalid weekday code: %r" % code)
    return WEEKDAY_CODES.index(code)


def week_start(day):
    """The Monday of the ISO week containing ``day`` (a date)."""
    if isinstance(day, datetime):
        day = day.date()
    return day - timedelta(days=day.weekday())


def start_of_day(day, tzinfo=None):
    """Midnight at the start of ``day`` as a datetime."""
    return datetime(day.year, day.month, day.day, tzinfo=tzinfo)


def daterange(start, end):
    """Yield every date in the half-open range ``[start, end)``."""
    day = start
    while day < end:
        yield day
        day += timedelta(days=1)


def minutes_between(a, b):
    """Whole minutes from ``a`` to ``b`` (negative when ``b`` is earlier).

    Seconds are truncated towards zero.
    """
    check_comparable(a, b)
    seconds = (b - a).total_seconds()
    return int(seconds / 60)


def format_minutes(minutes):
    """Format a minute count as ``<h>h<mm>m`` (``570`` -> ``9h30m``)."""
    sign = "-" if minutes < 0 else ""
    minutes = abs(int(minutes))
    return "%s%dh%02dm" % (sign, minutes // 60, minutes % 60)


def format_datetime(dt):
    """ISO-like text for a datetime, the inverse of :func:`parse_datetime`."""
    text = dt.strftime("%Y-%m-%dT%H:%M")
    if dt.second:
        text += ":%02d" % dt.second
    offset = dt.utcoffset()
    if offset is not None:
        if offset == timedelta(0):
            return text + "Z"
        total = int(offset.total_seconds() // 60)
        sign = "+" if total >= 0 else "-"
        total = abs(total)
        text += "%s%02d:%02d" % (sign, total // 60, total % 60)
    return text


def is_aware(dt):
    """True when ``dt`` carries a UTC offset."""
    return dt.tzinfo is not None and dt.utcoffset() is not None


def check_comparable(*values):
    """Raise :class:`ParseError` when naive and aware datetimes are mixed."""
    kinds = set(is_aware(v) for v in values if isinstance(v, datetime))
    if len(kinds) > 1:
        raise ParseError("cannot mix naive and timezone-aware datetimes")
