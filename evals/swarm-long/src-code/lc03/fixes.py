# Reference fixes for lc03 (rota). Exact, unique text replacements on the broken workspace.

FIXES = {
    1: [
        ("rota/recurrence.py",
         """                for wd in days:
                    yield base + timedelta(days=wd)
""",
         """                for wd in days:
                    occ = base + timedelta(days=wd)
                    if occ >= dtstart:
                        yield occ
"""),
        ("rota/recurrence.py",
         """            if self.until is not None and occ >= self.until:
                return
""",
         """            if self.until is not None and occ > self.until:
                return
"""),
        ("rota/recurrence.py",
         """        for occ in self._candidates(dtstart):
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
""",
         """        for occ in self.iter_occurrences(dtstart):
            if occ >= end:
                break
            if occ >= start:
                out.append(occ)
        return out
"""),
    ],
    2: [
        ("rota/holidays.py",
         """        if day.weekday() == 5:
            return day + timedelta(days=2)
""",
         """        if day.weekday() == 5:
            return day - timedelta(days=1)
"""),
        ("rota/calendar.py",
         """            table = {}
            for rule in self.rules:
                day = rule.observed_date(year)
                if day is not None:
                    table.setdefault(day, rule.name)
""",
         """            table = {}
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
"""),
    ],
    3: [
        ("rota/conflicts.py",
         """        items = groups[emp]
        for a, b in zip(items, items[1:]):
            if b.start < a.end:
                continue
            gap = minutes_between(a.start, b.start)
            if gap <= limit:
""",
         """        items = sorted(groups[emp], key=lambda s: (s.start, s.shift_id))
        for a, b in zip(items, items[1:]):
            if b.start < a.end:
                continue
            gap = minutes_between(a.end, b.start)
            if gap < limit:
"""),
    ],
    4: [
        ("rota/models.py",
         """            if off.start <= end and start <= off.end:
""",
         """            if off.start < end and start < off.end:
"""),
        ("rota/assign.py",
         """    if not employee.can_work(shift.role):
        return False
""",
         """    if not employee.can_work(shift.role):
        return False
    if not employee.is_available(shift.start, shift.end):
        return False
"""),
        ("rota/assign.py",
         """        candidates.sort(key=lambda e: (len(booked[e.emp_id]), e.name))
""",
         """        candidates.sort(key=lambda e: (result.minutes[e.emp_id], e.emp_id))
"""),
    ],
    5: [
        ("rota/ical.py",
         '    return (value.replace(",", "\\\\,").replace(";", "\\\\;")\n            .replace("\\n", "\\\\n").replace("\\\\", "\\\\\\\\"))\n',
         '    return (value.replace("\\\\", "\\\\\\\\").replace(",", "\\\\,").replace(";", "\\\\;")\n            .replace("\\n", "\\\\n"))\n'),
        ("rota/ical.py",
         """    if len(line) <= limit:
        return line
    parts = [line[i:i + limit] for i in range(0, len(line), limit)]
    return (CRLF + " ").join(parts)
""",
         """    if len(line.encode("utf-8")) <= limit:
        return line
    parts = []
    current = ""
    size = 0
    budget = limit
    for ch in line:
        width = len(ch.encode("utf-8"))
        if size + width > budget:
            parts.append(current)
            current = ""
            size = 0
            budget = limit - 1
        current += ch
        size += width
    parts.append(current)
    return (CRLF + " ").join(parts)
"""),
        ("rota/ical.py",
         """        return value.strftime("%Y%m%dT%H%M%S") + "Z"
""",
         """        return value.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%S") + "Z"
"""),
    ],
    6: [
        ("rota/intervals.py",
         """        return Interval(max(self.start, lo), self.end)
""",
         """        return Interval(max(self.start, lo), min(self.end, hi))
"""),
        ("rota/report.py",
         """from datetime import timedelta
""",
         """from datetime import timedelta
from decimal import ROUND_HALF_UP, Decimal
"""),
        ("rota/report.py",
         """    totals = {}
    for shift in shifts:
        if shift.employee is None:
            continue
        minutes = minutes_in_window(shift, lo, hi)
        if minutes:
            totals[shift.employee] = totals.get(shift.employee, 0) + minutes
    return [(emp, round(minutes / 60.0, 1)) for emp, minutes in sorted(totals.items())]
""",
         """    totals = dict((e.emp_id, 0) for e in employees)
    for shift in shifts:
        if shift.employee is None or shift.employee not in totals:
            continue
        totals[shift.employee] += minutes_in_window(shift, lo, hi)
    rows = [(emp, (Decimal(minutes) / Decimal(60)).quantize(Decimal("0.1"), rounding=ROUND_HALF_UP))
            for emp, minutes in totals.items()]
    return sorted(rows, key=lambda row: (-row[1], row[0]))
"""),
    ],
    7: [
        ("rota/availability.py",
         """from .intervals import Interval, subtract
""",
         """from .intervals import Interval, merge_intervals, subtract
"""),
        ("rota/availability.py",
         """    lists = [sorted(ws) for _, ws in sorted(windows_by_person.items())]
""",
         """    lists = [merge_intervals(ws) for _, ws in sorted(windows_by_person.items())]
"""),
        ("rota/availability.py",
         """        if start + duration < end:
""",
         """        if start + duration <= end:
"""),
    ],
}
