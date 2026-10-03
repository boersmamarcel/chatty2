"""Working calendars: which days are worked and during which hours.

A :class:`WorkingCalendar` combines a set of working weekdays, daily working
hours and public holidays (rules from :mod:`rota.holidays` plus one-off extra
dates).  A holiday is never a working day, even when it falls on a working
weekday.
"""

from datetime import datetime, time, timedelta

from .errors import CalendarError
from .holidays import parse_rules
from .timeutil import parse_date, parse_time, weekday_index


class WorkingCalendar(object):
    """Working days and hours of a site.

    :param workdays: weekday indexes (0=Monday) that are worked.
    :param rules: list of :class:`rota.holidays.HolidayRule`.
    :param extra_holidays: list of ``(date, name)`` one-off closures.
    :param day_start, day_end: working hours of every working day.
    """

    def __init__(self, workdays=(0, 1, 2, 3, 4), rules=(), extra_holidays=(),
                 day_start=time(9, 0), day_end=time(17, 0)):
        if not workdays:
            raise CalendarError("a calendar needs at least one working weekday")
        if day_end <= day_start:
            raise CalendarError("working hours must end after they start")
        self.workdays = frozenset(workdays)
        self.rules = list(rules)
        self.extra = list(extra_holidays)
        self.day_start = day_start
        self.day_end = day_end
        self._cache = {}

    @classmethod
    def from_config(cls, config):
        """Build a calendar from a plain dict, e.g. loaded from JSON::

            {"workdays": ["MO", "TU", "WE", "TH", "FR"],
             "hours": "08:30-17:00",
             "holidays": "Christmas Day = fixed 12-25 observed",
             "closures": {"2026-08-14": "Stocktake"}}
        """
        workdays = [weekday_index(c) for c in config.get("workdays", ["MO", "TU", "WE", "TH", "FR"])]
        hours = config.get("hours", "09:00-17:00")
        try:
            start_text, end_text = hours.split("-")
        except ValueError:
            raise CalendarError("hours must look like 09:00-17:00: %r" % hours)
        rules = parse_rules(config.get("holidays", ""))
        extra = sorted((parse_date(k), v) for k, v in config.get("closures", {}).items())
        return cls(workdays=workdays, rules=rules, extra_holidays=extra,
                   day_start=parse_time(start_text), day_end=parse_time(end_text))

    # ------------------------------------------------------------ holidays

    def _holidays(self, year):
        """``{date: name}`` of every holiday in calendar year ``year``."""
        if year not in self._cache:
            table = {}
            for rule_year in (year - 1, year, year + 1):
                for rule in self.rules:
                    actual = rule.actual(rule_year)
                    if actual is None:
                        continue
                    observed = rule.observed_date(rule_year)
                    if actual.year == year:
                        table.setdefault(actual, rule.name)
                    if observed != actual and observed.year == year:
                        table.setdefault(observed, rule.name + " (observed)")
            for day, name in self.extra:
                if day.year == year:
                    table.setdefault(day, name)
            self._cache[year] = table
        return self._cache[year]

    def holiday_name(self, day):
        """Name of the holiday on ``day``, or ``None`` when it is no holiday."""
        return self._holidays(day.year).get(day)

    def holidays_between(self, start, end):
        """Sorted ``(date, name)`` holidays in the half-open range ``[start, end)``."""
        out = []
        for year in range(start.year, end.year + 1):
            for day, name in self._holidays(year).items():
                if start <= day < end:
                    out.append((day, name))
        return sorted(out)

    # ------------------------------------------------------------ days

    def is_working_day(self, day):
        """True when ``day`` is a working weekday and not a holiday."""
        return day.weekday() in self.workdays and self.holiday_name(day) is None

    def next_working_day(self, day, include_today=False):
        """The first working day after ``day`` (or ``day`` itself if allowed)."""
        if include_today and self.is_working_day(day):
            return day
        current = day + timedelta(days=1)
        for _ in range(3660):
            if self.is_working_day(current):
                return current
            current += timedelta(days=1)
        raise CalendarError("no working day within ten years of %s" % day)

    def previous_working_day(self, day):
        """The last working day before ``day``."""
        current = day - timedelta(days=1)
        for _ in range(3660):
            if self.is_working_day(current):
                return current
            current -= timedelta(days=1)
        raise CalendarError("no working day within ten years before %s" % day)

    def add_working_days(self, day, n):
        """Move ``n`` working days from ``day`` (backwards when ``n`` < 0).

        ``n == 0`` returns ``day`` if it is a working day, else the next one.
        """
        if n == 0:
            return self.next_working_day(day, include_today=True)
        current = day
        step = self.next_working_day if n > 0 else self.previous_working_day
        for _ in range(abs(n)):
            current = step(current)
        return current

    def working_days_between(self, start, end):
        """Number of working days in the half-open range ``[start, end)``."""
        count = 0
        current = start
        while current < end:
            if self.is_working_day(current):
                count += 1
            current += timedelta(days=1)
        return count

    # ------------------------------------------------------------ hours

    def working_hours(self, day):
        """``(start, end)`` datetimes of the working hours of ``day`` or ``None``."""
        if not self.is_working_day(day):
            return None
        return (datetime.combine(day, self.day_start), datetime.combine(day, self.day_end))

    def working_minutes(self, start, end):
        """Working minutes between two naive datetimes.

        Only time inside the working hours of working days counts.
        """
        if end <= start:
            return 0
        total = 0
        day = start.date()
        while day <= end.date():
            hours = self.working_hours(day)
            if hours is not None:
                lo = max(start, hours[0])
                hi = min(end, hours[1])
                if hi > lo:
                    total += int((hi - lo).total_seconds() // 60)
            day += timedelta(days=1)
        return total
