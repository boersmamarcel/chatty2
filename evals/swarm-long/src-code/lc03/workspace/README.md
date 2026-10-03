# rota

Shift and meeting scheduling for small sites (wards, depots, help desks).
Pure Python standard library, no third-party dependencies.

What it does:

- recurrence rules (a DAILY/WEEKLY/MONTHLY subset of iCalendar RRULE) — `rota.recurrence`
- public holiday rules and working calendars — `rota.holidays`, `rota.calendar`
- shift templates expanded into concrete shifts — `rota.templates`
- greedy rota assignment with skills, time off, rest time and weekly caps — `rota.assign`
- conflict detection (overlaps, rest time, weekly hours) — `rota.conflicts`
- availability windows and meeting slot search — `rota.availability`, `rota.meetings`
- plain-text reports — `rota.report`
- iCalendar export — `rota.ical`

Times are naive local datetimes or datetimes with a fixed UTC offset
(`datetime.timezone`); the package never uses a timezone database.
Intervals are half-open: `[start, end)`.

Run the tests:

    python3 -m unittest discover -s tests -t .
