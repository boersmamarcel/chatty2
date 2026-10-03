"""Exception hierarchy of the rota package.

Every error raised on purpose by the package derives from :class:`RotaError`
so callers can catch the whole family with one ``except`` clause.  Parsing
errors additionally derive from :class:`ValueError` because they are, in the
end, bad input values.
"""


class RotaError(Exception):
    """Base class of every error raised by the rota package."""


class ParseError(RotaError, ValueError):
    """Text input (dates, times, rules, templates) could not be parsed."""


class RecurrenceError(RotaError, ValueError):
    """A recurrence rule is malformed or inconsistent."""


class CalendarError(RotaError):
    """A working calendar was configured or queried inconsistently."""


class AssignmentError(RotaError):
    """The rota assignment could not be carried out."""


class ExportError(RotaError):
    """An object could not be exported to iCalendar text."""
