"""iCalendar (RFC 5545) export of shifts and meetings.

Only what calendar clients need to import a rota is produced: one
``VCALENDAR`` with a ``VEVENT`` per shift or meeting.  Rules followed:

* lines are separated by CRLF and the text ends with a CRLF;
* TEXT values (``SUMMARY``, ``LOCATION``, ``DESCRIPTION``) escape backslash,
  semicolon, comma and newlines;
* content lines longer than 75 octets are folded;
* naive datetimes are written as floating local time (``20260301T090000``),
  aware datetimes are converted to UTC and written with a ``Z`` suffix.
"""

from datetime import timezone

from .errors import ExportError
from .models import Meeting, Shift

CRLF = "\r\n"


def escape_text(value):
    """Escape a TEXT property value."""
    value = value.replace("\r\n", "\n")
    return (value.replace(",", "\\,").replace(";", "\\;")
            .replace("\n", "\\n").replace("\\", "\\\\"))


def fold_line(line, limit=75):
    """Fold one content line so that no physical line exceeds ``limit``."""
    if len(line) <= limit:
        return line
    parts = [line[i:i + limit] for i in range(0, len(line), limit)]
    return (CRLF + " ").join(parts)


def format_dt(value):
    """DATE-TIME value: floating for naive, UTC with ``Z`` for aware values."""
    if value.tzinfo is not None and value.utcoffset() is not None:
        return value.strftime("%Y%m%dT%H%M%S") + "Z"
    return value.strftime("%Y%m%dT%H%M%S")


class Event(object):
    """The exportable view of a shift or meeting."""

    def __init__(self, uid, summary, start, end, location="", description="",
                 rrule=None, attendees=()):
        if not uid:
            raise ExportError("event needs a UID")
        self.uid = uid
        self.summary = summary
        self.start = start
        self.end = end
        self.location = location
        self.description = description
        self.rrule = rrule
        self.attendees = list(attendees)

    @classmethod
    def from_shift(cls, shift, domain="rota.local", names=None):
        """Event for a shift; ``names`` maps employee id to display name."""
        who = shift.employee or "unassigned"
        if names and shift.employee in names:
            who = names[shift.employee]
        summary = "%s shift (%s)" % (shift.role, who)
        return cls("%s@%s" % (shift.shift_id, domain), summary, shift.start, shift.end,
                   location=shift.location)

    @classmethod
    def from_meeting(cls, meeting, domain="rota.local"):
        """Event for a meeting (keeps its recurrence rule and attendees)."""
        return cls("%s@%s" % (meeting.uid, domain), meeting.title, meeting.start, meeting.end,
                   location=meeting.location, description=meeting.description,
                   rrule=meeting.rrule, attendees=meeting.attendees)

    def lines(self, dtstamp):
        """Unfolded content lines of the VEVENT."""
        out = ["BEGIN:VEVENT",
               "UID:" + self.uid,
               "DTSTAMP:" + format_dt(dtstamp),
               "DTSTART:" + format_dt(self.start),
               "DTEND:" + format_dt(self.end),
               "SUMMARY:" + escape_text(self.summary)]
        if self.location:
            out.append("LOCATION:" + escape_text(self.location))
        if self.description:
            out.append("DESCRIPTION:" + escape_text(self.description))
        if self.rrule:
            rule = self.rrule
            if rule.upper().startswith("RRULE:"):
                rule = rule[6:]
            out.append("RRULE:" + rule)
        for address in self.attendees:
            out.append("ATTENDEE:mailto:" + address)
        out.append("END:VEVENT")
        return out


def _as_event(item, domain, names):
    if isinstance(item, Event):
        return item
    if isinstance(item, Shift):
        return Event.from_shift(item, domain, names)
    if isinstance(item, Meeting):
        return Event.from_meeting(item, domain)
    raise ExportError("cannot export %r" % (item,))


def export_calendar(items, dtstamp, prodid="-//rota//EN", domain="rota.local", names=None):
    """Return the iCalendar text for shifts, meetings or events.

    ``dtstamp`` (a datetime) is written as the DTSTAMP of every event so the
    output is reproducible.  Events appear in the order given.
    """
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:" + prodid, "CALSCALE:GREGORIAN"]
    for item in items:
        lines.extend(_as_event(item, domain, names).lines(dtstamp))
    lines.append("END:VCALENDAR")
    return CRLF.join(fold_line(line) for line in lines) + CRLF


def unfold(text):
    """Undo line folding (useful for tests and for reading exports back)."""
    return text.replace(CRLF + " ", "").replace(CRLF + "\t", "")
