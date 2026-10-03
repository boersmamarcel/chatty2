"""rota: shift and meeting scheduling.

Modules:

* :mod:`rota.timeutil`     parsing and formatting of dates, times, durations
* :mod:`rota.intervals`    half-open time intervals
* :mod:`rota.recurrence`   RRULE subset (DAILY/WEEKLY/MONTHLY)
* :mod:`rota.holidays`     public holiday rules
* :mod:`rota.calendar`     working calendars (days, hours, holidays)
* :mod:`rota.models`       employees, shifts, meetings
* :mod:`rota.templates`    shift templates and their expansion
* :mod:`rota.assign`       greedy automatic rota assignment
* :mod:`rota.conflicts`    overlap / rest / weekly-hours conflict detection
* :mod:`rota.availability` availability windows and meeting slot search
* :mod:`rota.meetings`     recurring meetings and booking
* :mod:`rota.report`       plain-text reports
* :mod:`rota.ical`         iCalendar export
"""

__version__ = "0.4.2"
