"""Recurring meetings: expansion, holiday skipping and slot booking."""

from datetime import timedelta

from .availability import find_slot
from .models import Meeting
from .recurrence import Recurrence


class MeetingSeries(object):
    """A meeting that repeats according to a :class:`Recurrence`.

    :param start: start of the first occurrence (the rule's DTSTART).
    :param duration_minutes: length of each occurrence.
    :param rule: a :class:`Recurrence` or its text form.
    """

    def __init__(self, uid, title, start, duration_minutes, rule, attendees=(), location=""):
        if duration_minutes <= 0:
            raise ValueError("duration must be positive")
        if isinstance(rule, str):
            rule = Recurrence.parse(rule)
        self.uid = uid
        self.title = title
        self.start = start
        self.duration = timedelta(minutes=duration_minutes)
        self.rule = rule
        self.attendees = list(attendees)
        self.location = location

    def occurrences(self, window_start, window_end, calendar=None):
        """Concrete meetings starting in ``[window_start, window_end)``.

        With a calendar, occurrences on non-working days are dropped (they
        still count towards the rule's COUNT).  Each occurrence gets the uid
        ``<uid>-<YYYYMMDD>``.
        """
        out = []
        for start in self.rule.between(self.start, window_start, window_end):
            if calendar is not None and not calendar.is_working_day(start.date()):
                continue
            out.append(Meeting("%s-%s" % (self.uid, start.strftime("%Y%m%d")), self.title,
                               start, start + self.duration, self.attendees, self.location))
        return out

    def as_meeting(self):
        """The whole series as one :class:`Meeting` carrying an RRULE (for export)."""
        return Meeting(self.uid, self.title, self.start, self.start + self.duration,
                       self.attendees, self.location, rrule=self.rule.to_string())


def book_meeting(availability, people, uid, title, duration_minutes, earliest=None,
                 latest=None, granularity=15):
    """Find the earliest common slot, block it for everybody and return a Meeting.

    Returns ``None`` (and blocks nothing) when no slot exists.
    """
    slot = find_slot(availability.as_dict(people), duration_minutes, earliest, latest,
                     granularity)
    if slot is None:
        return None
    for person in people:
        availability.block(person, slot.start, slot.end)
    return Meeting(uid, title, slot.start, slot.end, attendees=people)
