"""Service-day clock times.

Timetables count time in seconds since the start of the *service day*, not
since midnight: a trip that leaves at 23:50 and arrives after midnight is
written ``23:50`` -> ``24:20`` so that times along one trip keep increasing.
Times are written ``HH:MM`` or ``HH:MM:SS`` with two-digit minutes and
seconds.
"""

import re

_TIME = re.compile(r"^(\d{1,2}):(\d{2})(?::(\d{2}))?$")

#: Hours accepted by :func:`parse_time` are ``0 .. MAX_HOURS - 1``.
MAX_HOURS = 24

SECONDS_PER_MINUTE = 60
SECONDS_PER_HOUR = 3600


def parse_time(text):
    """Parse ``"HH:MM"`` or ``"HH:MM:SS"`` into seconds since service-day start.

    Surrounding whitespace is ignored.  Raises ``ValueError`` for anything
    else (bad shape, minutes or seconds above 59, hours out of range).
    """
    if not isinstance(text, str):
        raise ValueError("bad time %r" % (text,))
    match = _TIME.match(text.strip())
    if match is None:
        raise ValueError("bad time %r" % (text,))
    hours = int(match.group(1))
    minutes = int(match.group(2))
    seconds = int(match.group(3) or 0)
    if hours >= MAX_HOURS or minutes > 59 or seconds > 59:
        raise ValueError("bad time %r" % (text,))
    return hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE + seconds


def format_time(seconds):
    """Inverse of :func:`parse_time`.

    ``HH:MM`` when the seconds part is zero, ``HH:MM:SS`` otherwise.  Raises
    ``ValueError`` for negative values.
    """
    if seconds < 0:
        raise ValueError("negative time %r" % (seconds,))
    hours, rest = divmod(int(seconds), SECONDS_PER_HOUR)
    minutes, secs = divmod(rest, SECONDS_PER_MINUTE)
    hours = hours % 24
    if secs:
        return "%02d:%02d:%02d" % (hours, minutes, secs)
    return "%02d:%02d" % (hours, minutes)


def parse_optional_time(text):
    """Like :func:`parse_time` but an empty/blank string gives ``None``."""
    if text is None or not text.strip():
        return None
    return parse_time(text)


def minutes_between(start, end):
    """Whole minutes from ``start`` to ``end`` (both in seconds), rounded down."""
    if end < start:
        raise ValueError("end %r is before start %r" % (end, start))
    return (end - start) // SECONDS_PER_MINUTE


def add_minutes(seconds, minutes):
    """``seconds`` shifted by a whole number of minutes."""
    return seconds + minutes * SECONDS_PER_MINUTE
