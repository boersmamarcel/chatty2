"""Public holiday rules.

A rule describes how to find a holiday's date in a given year:

``fixed``
    the same month and day every year (``12-25``)
``nth``
    the n-th given weekday of a month (``1 MO 05`` = first Monday of May)
``last``
    the last given weekday of a month (``MO 05`` = last Monday of May)
``easter``
    an offset in days from Western Easter Sunday (``-2`` = Good Friday)

Rules marked ``observed`` follow the weekend substitution convention: when
the holiday falls on a Saturday it is observed on the Friday before, when it
falls on a Sunday it is observed on the Monday after.  Other rules are never
moved.

Text form, one rule per line (``#`` starts a comment)::

    New Year's Day   = fixed 01-01 observed
    Early May        = nth 1 MO 05
    Spring Bank      = last MO 05
    Good Friday      = easter -2
"""

import calendar as _cal
from datetime import date, timedelta

from .errors import ParseError
from .timeutil import WEEKDAY_CODES, weekday_index

KINDS = ("fixed", "nth", "last", "easter")


def easter_sunday(year):
    """Western (Gregorian) Easter Sunday, anonymous Gregorian algorithm."""
    a = year % 19
    b, c = divmod(year, 100)
    d, e = divmod(b, 4)
    f = (b + 8) // 25
    g = (b - f + 1) // 3
    h = (19 * a + b - d - g + 15) % 30
    i, k = divmod(c, 4)
    l = (32 + 2 * e + 2 * i - h - k) % 7
    m = (a + 11 * h + 22 * l) // 451
    month, day = divmod(h + l - 7 * m + 114, 31)
    return date(year, month, day + 1)


def nth_weekday(year, month, weekday, n):
    """The ``n``-th (1-based) ``weekday`` (0=Monday) of a month, or ``None``."""
    first = date(year, month, 1)
    offset = (weekday - first.weekday()) % 7
    day = 1 + offset + 7 * (n - 1)
    if day > _cal.monthrange(year, month)[1]:
        return None
    return date(year, month, day)


def last_weekday(year, month, weekday):
    """The last ``weekday`` (0=Monday) of a month."""
    last = date(year, month, _cal.monthrange(year, month)[1])
    return last - timedelta(days=(last.weekday() - weekday) % 7)


class HolidayRule(object):
    """One holiday definition; see the module docstring."""

    def __init__(self, name, kind, month=None, day=None, weekday=None, n=None,
                 offset=0, observed=False):
        if kind not in KINDS:
            raise ParseError("unknown holiday kind: %r" % kind)
        if kind in ("fixed", "nth", "last") and not (month and 1 <= month <= 12):
            raise ParseError("holiday %r needs a month 1-12" % name)
        if kind == "fixed" and not (day and 1 <= day <= 31):
            raise ParseError("holiday %r needs a day 1-31" % name)
        if kind in ("nth", "last") and weekday is None:
            raise ParseError("holiday %r needs a weekday" % name)
        if kind == "nth" and not (n and 1 <= n <= 5):
            raise ParseError("holiday %r needs n between 1 and 5" % name)
        self.name = name
        self.kind = kind
        self.month = month
        self.day = day
        self.weekday = weekday
        self.n = n
        self.offset = offset
        self.observed = observed

    def actual(self, year):
        """The calendar date of the holiday in ``year`` (``None`` if none)."""
        if self.kind == "fixed":
            if self.day > _cal.monthrange(year, self.month)[1]:
                return None
            return date(year, self.month, self.day)
        if self.kind == "nth":
            return nth_weekday(year, self.month, self.weekday, self.n)
        if self.kind == "last":
            return last_weekday(year, self.month, self.weekday)
        return easter_sunday(year) + timedelta(days=self.offset)

    def observed_date(self, year):
        """The date the holiday is observed for the occurrence of ``year``.

        Equal to :meth:`actual` unless the rule is ``observed`` and the
        holiday falls on a weekend.  The result may lie in another year.
        """
        day = self.actual(year)
        if day is None or not self.observed:
            return day
        if day.weekday() == 5:
            return day - timedelta(days=1)
        if day.weekday() == 6:
            return day + timedelta(days=1)
        return day

    def to_string(self):
        """Text form accepted by :func:`parse_rule`."""
        if self.kind == "fixed":
            body = "fixed %02d-%02d" % (self.month, self.day)
        elif self.kind == "nth":
            body = "nth %d %s %02d" % (self.n, WEEKDAY_CODES[self.weekday], self.month)
        elif self.kind == "last":
            body = "last %s %02d" % (WEEKDAY_CODES[self.weekday], self.month)
        else:
            body = "easter %+d" % self.offset
        if self.observed:
            body += " observed"
        return "%s = %s" % (self.name, body)

    def __repr__(self):
        return "HolidayRule(%r)" % self.to_string()


def parse_rule(line):
    """Parse one rule line (see the module docstring)."""
    if "=" not in line:
        raise ParseError("holiday rule needs 'name = definition': %r" % line)
    name, body = line.split("=", 1)
    name = name.strip()
    words = body.split()
    if not name or not words:
        raise ParseError("empty holiday rule: %r" % line)
    observed = False
    if words[-1].lower() == "observed":
        observed = True
        words = words[:-1]
    kind = words[0].lower() if words else ""
    try:
        if kind == "fixed" and len(words) == 2:
            month, day = words[1].split("-")
            return HolidayRule(name, kind, month=int(month), day=int(day), observed=observed)
        if kind == "nth" and len(words) == 4:
            return HolidayRule(name, kind, n=int(words[1]), weekday=weekday_index(words[2]),
                               month=int(words[3]), observed=observed)
        if kind == "last" and len(words) == 3:
            return HolidayRule(name, kind, weekday=weekday_index(words[1]),
                               month=int(words[2]), observed=observed)
        if kind == "easter" and len(words) == 2:
            return HolidayRule(name, kind, offset=int(words[1]), observed=observed)
    except ValueError:
        pass
    raise ParseError("invalid holiday rule: %r" % line)


def parse_rules(text):
    """Parse several rule lines; blank lines and ``#`` comments are skipped.

    The first bad line raises :class:`ParseError` with its 1-based number.
    """
    rules = []
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        try:
            rules.append(parse_rule(line))
        except ParseError as exc:
            raise ParseError("line %d: %s" % (number, exc))
    return rules


def holidays_for_year(rules, year):
    """``(actual date, name)`` pairs of every rule in ``year``, sorted by date."""
    found = []
    for rule in rules:
        day = rule.actual(year)
        if day is not None:
            found.append((day, rule.name))
    return sorted(found)
