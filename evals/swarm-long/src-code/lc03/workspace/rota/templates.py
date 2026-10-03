"""Shift templates and their expansion into concrete shifts.

A template line looks like::

    EARLY 06:00-14:00 nurse MO,TU,WE,TH,FR @Ward 3

i.e. a code, a time range (an end at or before the start means the shift
ends the next day), a role, the weekdays it runs on and an optional
``@location``.
"""

from datetime import datetime, timedelta

from .errors import ParseError
from .models import Shift
from .timeutil import WEEKDAY_CODES, daterange, parse_time, weekday_index


class ShiftTemplate(object):
    """A recurring shift pattern."""

    def __init__(self, code, start_time, end_time, role, weekdays=range(7), location=""):
        if not code or not code.replace("-", "").replace("_", "").isalnum():
            raise ParseError("invalid template code: %r" % code)
        self.code = code
        self.start_time = start_time
        self.end_time = end_time
        self.role = role
        self.weekdays = sorted(set(weekdays))
        self.location = location

    @classmethod
    def parse(cls, line):
        """Parse a template line (see the module docstring)."""
        location = ""
        if "@" in line:
            line, location = line.split("@", 1)
            location = location.strip()
        words = line.split()
        if len(words) not in (3, 4):
            raise ParseError("invalid template: %r" % line)
        code, hours, role = words[0], words[1], words[2]
        if "-" not in hours:
            raise ParseError("invalid template hours: %r" % hours)
        start_text, end_text = hours.split("-", 1)
        weekdays = range(7)
        if len(words) == 4:
            weekdays = [weekday_index(w) for w in words[3].split(",")]
        return cls(code, parse_time(start_text), parse_time(end_text), role, weekdays, location)

    @property
    def overnight(self):
        """True when the shift ends on the following day."""
        return self.end_time <= self.start_time

    @property
    def minutes(self):
        """Length of one instance in minutes."""
        start = self.start_time.hour * 60 + self.start_time.minute
        end = self.end_time.hour * 60 + self.end_time.minute
        if end <= start:
            end += 24 * 60
        return end - start

    def instantiate(self, day):
        """The concrete :class:`Shift` of this template starting on ``day``."""
        start = datetime.combine(day, self.start_time)
        end = datetime.combine(day, self.end_time)
        if self.overnight:
            end += timedelta(days=1)
        shift_id = "%s-%s" % (self.code, day.strftime("%Y%m%d"))
        return Shift(shift_id, start, end, self.role, self.location)

    def to_string(self):
        days = ",".join(WEEKDAY_CODES[d] for d in self.weekdays)
        text = "%s %s-%s %s %s" % (self.code, self.start_time.strftime("%H:%M"),
                                   self.end_time.strftime("%H:%M"), self.role, days)
        if self.location:
            text += " @" + self.location
        return text


def parse_templates(text):
    """Parse several template lines; blank lines and ``#`` comments are skipped."""
    out = []
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        try:
            out.append(ShiftTemplate.parse(line))
        except ParseError as exc:
            raise ParseError("line %d: %s" % (number, exc))
    codes = [t.code for t in out]
    if len(set(codes)) != len(codes):
        raise ParseError("duplicate template codes")
    return out


def expand(templates, start, end, calendar=None):
    """Concrete shifts for every template on every day in ``[start, end)``.

    With a :class:`rota.calendar.WorkingCalendar`, days that are holidays are
    skipped (weekends are governed by the templates' own weekdays).  The
    result is sorted by start time, then shift id.
    """
    shifts = []
    for day in daterange(start, end):
        if calendar is not None and calendar.holiday_name(day) is not None:
            continue
        for template in templates:
            if day.weekday() in template.weekdays:
                shifts.append(template.instantiate(day))
    return sorted(shifts, key=lambda s: (s.start, s.shift_id))
