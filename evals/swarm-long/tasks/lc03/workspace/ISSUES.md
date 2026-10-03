# Open issues

Each issue below is independent of the others. Where an issue states
acceptance criteria, every bullet is required.

---

## Issue 1: recurring meetings show wrong occurrences

Reported by the ward manager: a daily handover set up as
`FREQ=DAILY;UNTIL=20260305T090000` starting 2026-03-01 09:00 stops on
the 4th instead of the 5th; a `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=3` series that
starts on Wednesday 2026-03-04 shows a meeting on Monday 2026-03-02, before the
series even began; and the calendar view (which asks for one week at a time)
shows more meetings than COUNT allows.

Acceptance (`rota.recurrence.Recurrence`):

- `UNTIL` is inclusive: an occurrence starting exactly at the UNTIL instant
  belongs to the series. This holds for `iter_occurrences(dtstart)` and for
  `between(dtstart, start, end)`.
- No occurrence is ever earlier than `dtstart`. For `WEEKLY` rules with
  `BYDAY`, weekdays of the first week that fall before `dtstart` are skipped,
  and they do not count towards `COUNT`. Example: dtstart Wednesday
  2026-03-04 09:00, `FREQ=WEEKLY;BYDAY=MO,WE;COUNT=3` gives 03-04, 03-09,
  03-11 (all 09:00).
- `COUNT` bounds the series as a whole, not the window:
  `between(dtstart, start, end)` returns those occurrences of the series
  (the first COUNT ones) that start in `[start, end)`. Example:
  `FREQ=DAILY;COUNT=5` from 2026-03-01 09:00 with the window
  `[2026-03-03 00:00, 2026-03-10 00:00)` gives 03-03, 03-04, 03-05.
  The window end stays exclusive.

---

## Issue 2: observed holidays are wrong in the working calendar

Christmas 2021 fell on a Saturday. Our holiday rules are marked `observed`,
so the site was closed on Friday 2021-12-24, but the calendar treated Monday
2021-12-27 as the holiday. New Year's Day 2022 (also a Saturday) should have
closed the site on Friday 2021-12-31, and that did not show up at all.

Acceptance:

- `HolidayRule.observed_date(year)` (`rota.holidays`): for an `observed` rule,
  a holiday on a Saturday is observed on the Friday before, one on a Sunday on
  the Monday after; weekday holidays are not moved. Rules that are not
  `observed` are never moved. The observed date may fall in a different
  calendar year than the holiday (New Year's Day 2022 -> 2021-12-31).
- `WorkingCalendar` (`rota.calendar`) treats both the actual date and the
  observed date as holidays. `holiday_name(day)` returns the rule name for the
  actual date and the rule name followed by `" (observed)"` for the observed
  date when it differs, e.g. `"Christmas Day"` for 2021-12-25 and
  `"Christmas Day (observed)"` for 2021-12-24. When the two dates coincide the
  plain name is returned.
- Observed dates that fall in the previous calendar year count in that year:
  `holiday_name(date(2021, 12, 31))` is `"New Year's Day (observed)"` and
  2021-12-31 is not a working day. `is_working_day`, `add_working_days`,
  `working_days_between` and `holidays_between` all follow these rules.

---

## Issue 3: rest-time check misses short turnarounds

A nurse was rostered on an early shift (06:00-14:00) and the night shift of the
same day (22:00-06:00) and `rest_violations` reported nothing.

Acceptance (`rota.conflicts.rest_violations(shifts, min_rest_hours=11)`):

- Rest is measured from the **end** of a shift to the start of the same
  employee's next shift (shifts may run past midnight).
- A rest of exactly the minimum is allowed; anything shorter is a violation.
- Shifts may be passed in any order; each employee's shifts are considered in
  chronological order (ties by shift id), and the conflict's `first`/`second`
  are the earlier and the later shift.
- The message format is unchanged:
  `E1: only 8h00m rest between S1 and S2 (minimum 11h00m)` (rest and
  minimum both as `<h>h<mm>m`). Overlapping shifts are still left to
  `find_overlaps` and unassigned shifts are ignored.

---

## Issue 4: auto-assignment puts people on shifts during their leave

`assign_rota` rostered an employee on a shift in the middle of their annual
leave. Also the assignment is not balanced the way the module docstring
promises.

Acceptance:

- `Employee.is_available(start, end)` (`rota.models`) returns `False` exactly
  when `[start, end)` overlaps one of the employee's `time_off` intervals.
  Intervals are half-open: a shift that starts at the instant the time off
  ends, or ends at the instant it starts, does not collide.
- `assign_rota` (`rota.assign`) never assigns a shift to an employee who is not
  available for it; if nobody is, the shift is listed in `unfilled`.
- Among the eligible employees the one with the **fewest minutes** assigned so
  far in this run wins (not the fewest shifts); remaining ties go to the
  smallest employee id, compared as a string (`"E10"` beats `"E9"`), never by
  name. `result.minutes` keeps reporting the total minutes per employee.

---

## Issue 5: calendar clients reject or mangle our iCalendar export

Outlook refuses the export when a summary is long, Google shows `\\,` in
locations, and meetings entered with a UTC offset show up two hours late.

Acceptance (`rota.ical`):

- `escape_text(value)` escapes a backslash as `\\`, `;` as `\;`, `,` as `\,`
  and a newline (`\n` or `\r\n`) as `\n` (backslash, letter n). Each
  character is escaped exactly once, e.g. `a\b, c` becomes `a\\b\, c`.
- `fold_line(line, limit=75)` limits lines by **octets** of the UTF-8
  encoding, not by characters. A line of at most 75 octets is returned
  unchanged. Otherwise it is split greedily: the first physical line holds as
  many whole characters as fit in 75 octets, every continuation line is a
  single space followed by as many whole characters as fit in the remaining
  74 octets; physical lines are joined with CRLF. A multi-byte character is
  never split.
- `format_dt(value)`: naive datetimes stay floating (`20260301T090000`);
  aware datetimes are converted to UTC and written with a `Z`, e.g.
  2026-03-01 09:00 at +02:00 becomes `20260301T070000Z`. This applies to
  DTSTART, DTEND and DTSTAMP of `export_calendar`.

---

## Issue 6: weekly hours report double-counts night shifts and rounds oddly

The weekly hours report credits a Sunday night shift (Sunday 22:00 - Monday
06:00) with 8 hours in the week it starts in and 6 more in the next week. The
hours are also rounded inconsistently (2 h 15 min shows as 2.2) and staff
without shifts are missing from the report.

Acceptance:

- `Interval.clip(lo, hi)` (`rota.intervals`) returns the part of the interval
  inside `[lo, hi)` with **both** ends clamped, or `None` when they do not
  overlap (merely touching counts as not overlapping).
- `weekly_hours(shifts, employees, week_start)` (`rota.report`) counts only the
  part of each shift inside `[week_start 00:00, week_start + 7 days 00:00)`:
  the Sunday night shift above gives 2.0 hours to the earlier week and 6.0 to
  the later one.
- It returns one `(employee id, hours)` pair for **every** employee in
  `employees`, including those without shifts (`Decimal("0.0")`). Unassigned
  shifts and shifts of employees not in `employees` are ignored.
- `hours` is a `decimal.Decimal` with exactly one decimal place: minutes / 60
  rounded half-up (2 h 15 min -> `2.3`, 2 h 03 min -> `2.1`).
- Rows are sorted by the rounded hours, highest first; equal hours are ordered
  by employee id ascending.

---

## Issue 7: meeting slot finder says "no slot" although everybody is free

Alice entered her availability as two windows, 09:00-10:00 and 10:00-11:30;
Bob is free 09:00-12:00. `find_slot` finds no 90-minute slot.

Acceptance (`rota.availability`):

- A person's windows may be entered in any order and may overlap or touch;
  `common_windows` and `find_slot` treat them as the union of the time the
  person is free (09:00-10:00 plus 10:00-11:30 is one free period
  09:00-11:30). `common_windows` returns sorted intervals.
- A slot may end exactly at the end of a common free period, or exactly at
  `latest`.
- Unchanged: the slot start is the earliest time not before `earliest` that is
  a multiple of `granularity` minutes after midnight; if that leaves too little
  room in a free period, the next period is tried; `None` when nothing fits.
