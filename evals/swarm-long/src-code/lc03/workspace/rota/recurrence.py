"""A small subset of iCalendar recurrence rules (RFC 5545 ``RRULE``).

Supported parts:

* ``FREQ`` = ``DAILY``, ``WEEKLY`` or ``MONTHLY`` (required)
* ``INTERVAL`` = step between periods (default 1)
* ``BYDAY`` = comma separated weekday codes, ``WEEKLY`` only
* ``BYMONTHDAY`` = comma separated days of the month, ``MONTHLY`` only
* ``COUNT`` = total number of occurrences of the series
* ``UNTIL`` = last instant an occurrence may start at (inclusive); a
  date-only value (``YYYYMMDD``) means 23:59:59 of that day

``COUNT`` and ``UNTIL`` are mutually exclusive.  The series always starts at
``dtstart``: no occurrence is ever earlier than ``dtstart``, and ``dtstart``
itself is the first occurrence whenever it matches the rule.
"""

import calendar as _cal
from datetime import datetime, timedelta, timezone

from .errors import RecurrenceError
from .timeutil import WEEKDAY_CODES, check_comparable, weekday_index

FREQS = ("DAILY", "WEEKLY", "MONTHLY")

#: Safety limit on the number of periods examined for one series.
MAX_PERIODS = 100000


def _parse_until(text):
    """``YYYYMMDD``, ``YYYYMMDDTHHMMSS`` or the same with ``Z`` (UTC)."""
    text = text.strip()
    utc = text.endswith("Z")
    if utc:
        text = text[:-1]
    try:
        if len(text) == 8:
            value = datetime.strptime(text, "%Y%m%d").replace(hour=23, minute=59, second=59)
        elif len(text) == 15:
            value = datetime.strptime(text, "%Y%m%dT%H%M%S")
        else:
            raise ValueError(text)
    except ValueError:
        raise RecurrenceError("invalid UNTIL value: %r" % text)
    if utc:
        value = value.replace(tzinfo=timezone.utc)
    return value


def _format_until(value):
    if value.tzinfo is not None and value.utcoffset() is not None:
        return value.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return value.strftime("%Y%m%dT%H%M%S")


def _add_months(year, month, delta):
    index = year * 12 + (month - 1) + delta
    return index // 12, index % 12 + 1


class Recurrence(object):
    """A parsed recurrence rule; see the module docstring for the semantics."""

    def __init__(self, freq, interval=1, byday=None, bymonthday=None,
                 count=None, until=None):
        freq = freq.upper()
        if freq not in FREQS:
            raise RecurrenceError("unsupported FREQ: %r" % freq)
        if int(interval) < 1:
            raise RecurrenceError("INTERVAL must be at least 1")
        if count is not None and until is not None:
            raise RecurrenceError("COUNT and UNTIL are mutually exclusive")
        if count is not None and int(count) < 1:
            raise RecurrenceError("COUNT must be at least 1")
        if byday and freq != "WEEKLY":
            raise RecurrenceError("BYDAY is only supported with FREQ=WEEKLY")
        if bymonthday and freq != "MONTHLY":
            raise RecurrenceError("BYMONTHDAY is only supported with FREQ=MONTHLY")
        self.freq = freq
        self.interval = int(interval)
        self.byday = sorted(set(weekday_index(c) if isinstance(c, str) else int(c)
                                for c in (byday or ())))
        days = sorted(set(int(d) for d in (bymonthday or ())))
        for d in days:
            if not 1 <= d <= 31:
                raise RecurrenceError("BYMONTHDAY out of range: %d" % d)
        self.bymonthday = days
        self.count = int(count) if count is not None else None
        self.until = until

    # ------------------------------------------------------------ text form

    @classmethod
    def parse(cls, text):
        """Parse ``FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=6``.

        A leading ``RRULE:`` is ignored; keys are case-insensitive.
        """
        text = text.strip()
        if text.upper().startswith("RRULE:"):
            text = text[6:]
        if not text:
            raise RecurrenceError("empty recurrence rule")
        fields = {}
        for part in text.split(";"):
            if not part:
                continue
            if "=" not in part:
                raise RecurrenceError("malformed rule part: %r" % part)
            key, value = part.split("=", 1)
            key = key.strip().upper()
            if key in fields:
                raise RecurrenceError("duplicate rule part: %s" % key)
            fields[key] = value.strip()
        if "FREQ" not in fields:
            raise RecurrenceError("FREQ is required")
        unknown = set(fields) - {"FREQ", "INTERVAL", "BYDAY", "BYMONTHDAY", "COUNT", "UNTIL"}
        if unknown:
            raise RecurrenceError("unsupported rule parts: %s" % ", ".join(sorted(unknown)))
        try:
            interval = int(fields.get("INTERVAL", "1"))
            count = int(fields["COUNT"]) if "COUNT" in fields else None
            bymonthday = [int(x) for x in fields["BYMONTHDAY"].split(",")] \
                if "BYMONTHDAY" in fields else None
        except ValueError:
            raise RecurrenceError("non-numeric value in rule: %r" % text)
        byday = fields["BYDAY"].split(",") if "BYDAY" in fields else None
        until = _parse_until(fields["UNTIL"]) if "UNTIL" in fields else None
        return cls(fields["FREQ"], interval=interval, byday=byday,
                   bymonthday=bymonthday, count=count, until=until)

    def to_string(self):
        """Canonical text form (parts in a fixed order, defaults omitted)."""
        parts = ["FREQ=" + self.freq]
        if self.interval != 1:
            parts.append("INTERVAL=%d" % self.interval)
        if self.byday:
            parts.append("BYDAY=" + ",".join(WEEKDAY_CODES[d] for d in self.byday))
        if self.bymonthday:
            parts.append("BYMONTHDAY=" + ",".join(str(d) for d in self.bymonthday))
        if self.count is not None:
            parts.append("COUNT=%d" % self.count)
        if self.until is not None:
            parts.append("UNTIL=" + _format_until(self.until))
        return ";".join(parts)

    def __repr__(self):
        return "Recurrence(%r)" % self.to_string()

    # ------------------------------------------------------------ expansion

    def _candidates(self, dtstart):
        """Every date-time matching FREQ/INTERVAL/BY* in order, unbounded."""
        if self.freq == "DAILY":
            for k in range(MAX_PERIODS):
                yield dtstart + timedelta(days=k * self.interval)
        elif self.freq == "WEEKLY":
            days = self.byday or [dtstart.weekday()]
            monday = dtstart - timedelta(days=dtstart.weekday())
            for k in range(MAX_PERIODS):
                base = monday + timedelta(days=7 * k * self.interval)
                for wd in days:
                    yield base + timedelta(days=wd)
        else:
            days = self.bymonthday or [dtstart.day]
            for k in range(MAX_PERIODS):
                year, month = _add_months(dtstart.year, dtstart.month, k * self.interval)
                last = _cal.monthrange(year, month)[1]
                for d in days:
                    if d > last:
                        continue
                    occ = dtstart.replace(year=year, month=month, day=d)
                    if occ >= dtstart:
                        yield occ

    def iter_occurrences(self, dtstart):
        """Yield the occurrences of the series starting at ``dtstart``.

        The generator stops after ``COUNT`` occurrences or after the last
        occurrence not later than ``UNTIL``; without either it is unbounded.
        """
        if self.until is not None:
            check_comparable(dtstart, self.until)
        produced = 0
        for occ in self._candidates(dtstart):
            if self.until is not None and occ >= self.until:
                return
            yield occ
            produced += 1
            if self.count is not None and produced >= self.count:
                return

    def between(self, dtstart, start, end):
        """Occurrences of the series that fall in ``[start, end)``.

        ``COUNT`` and ``UNTIL`` bound the series as a whole, exactly as in
        :meth:`iter_occurrences`; the window only selects from that series.
        """
        check_comparable(dtstart, start, end)
        out = []
        for occ in self._candidates(dtstart):
            if self.until is not None and occ >= self.until:
                break
            if occ >= end:
                break
            if occ < start:
                continue
            out.append(occ)
            if self.count is not None and len(out) >= self.count:
                break
        return out

    def first_after(self, dtstart, instant):
        """The first occurrence strictly after ``instant`` (or ``None``)."""
        for occ in self.iter_occurrences(dtstart):
            if occ > instant:
                return occ
        return None

    def is_finite(self):
        """True when the series has a COUNT or an UNTIL."""
        return self.count is not None or self.until is not None
