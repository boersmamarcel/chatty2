"""Reference fixes for lc07 (gradebook): exact replacements per issue."""

FIXES = {
    1: [
        ("gradebook/grading.py",
         """    for threshold, letter in SCALE:
        if pct > threshold:
            return letter
    return "F\"""",
         """    if pct < 0:
        raise GradingError("negative percentage: %r" % (pct,))
    rounded = round_half_up(pct, 1)
    for threshold, letter in SCALE:
        if rounded >= threshold:
            return letter
    return "F\""""),
    ],
    2: [
        ("gradebook/policy.py",
         """    if n <= 0:
        return list(entries)
    ranked = sorted(entries, key=lambda entry: entry[1])
    return ranked[n:]""",
         """    if n < 0:
        raise PolicyError("cannot drop a negative number of results: %r" % (n,))
    entries = list(entries)
    if n == 0 or not entries:
        return entries
    n = min(n, len(entries) - 1)

    def weakness(index):
        assessment, points = entries[index]
        due = (0, assessment.due) if assessment.due is not None else (1, None)
        return (points / assessment.max_points, due, assessment.key)

    dropped = set(sorted(range(len(entries)), key=weakness)[:n])
    return [entry for index, entry in enumerate(entries) if index not in dropped]"""),
    ],
    3: [
        ("gradebook/late.py",
         """    if delta <= datetime.timedelta(0):
        return 0
    return delta.days""",
         """    if delta <= datetime.timedelta(0):
        return 0
    days = delta.days
    if delta - datetime.timedelta(days=days) > datetime.timedelta(0):
        days += 1
    return days"""),
        ("gradebook/late.py",
         """    days = days_late(due, submitted)
    if days == 0:
        return points
    penalty = points * RATE_PER_DAY * days
    return points - penalty""",
         """    days = min(days_late(effective_due(due, extension_days), submitted), MAX_PENALTY_DAYS)
    if days == 0:
        return points
    penalty = max_points * RATE_PER_DAY * days
    return max(0.0, points - penalty)"""),
    ],
    4: [
        ("gradebook/loader.py",
         """from .models import Assessment, Mark, Student, parse_datetime""",
         """from .models import EXCUSED, Assessment, Mark, Student, parse_datetime"""),
        ("gradebook/loader.py",
         """    \"\"\"Points of one marks cell (a float).\"\"\"
    return float(text)""",
         """    \"\"\"Points of one marks cell: a float, None (missing) or EXCUSED.\"\"\"
    text = text.strip()
    if not text:
        return None
    if text.upper() == "EX":
        return EXCUSED
    if text.count(",") == 1 and "." not in text:
        text = text.replace(",", ".")
    try:
        points = float(text)
    except ValueError:
        raise LoaderError("bad points %r" % (text,))
    if points < 0:
        raise LoaderError("negative points %r" % (text,))
    return points"""),
        ("gradebook/loader.py",
         """    for number, row in enumerate(_rows(text, MARK_COLUMNS, "marks"), 1):""",
         """    for number, row in enumerate(_rows(text, MARK_COLUMNS, "marks"), 2):"""),
    ],
    5: [
        ("gradebook/report.py",
         """    ordered = sorted(values)
    return ordered[len(ordered) // 2]""",
         """    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2 == 0:
        return (ordered[middle - 1] + ordered[middle]) / 2
    return ordered[middle]"""),
        ("gradebook/report.py",
         """    values = list(values)
    centre = mean(values)
    return math.sqrt(sum((v - centre) ** 2 for v in values) / len(values))""",
         """    values = list(values)
    if len(values) < 2:
        return 0.0
    centre = mean(values)
    return math.sqrt(sum((v - centre) ** 2 for v in values) / (len(values) - 1))"""),
        ("gradebook/report.py",
         """    values = list(percentages)
    return {""",
         """    values = list(percentages)
    if not values:
        raise ValueError("no percentages")
    return {"""),
        ("gradebook/report.py",
         """    return [(index + 1, name, pct, letter) for index, (name, pct, letter) in enumerate(ordered)]""",
         """    ranked = []
    for index, (name, pct, letter) in enumerate(ordered):
        if index and pct == ordered[index - 1][1]:
            rank = ranked[-1][0]
        else:
            rank = index + 1
        ranked.append((rank, name, pct, letter))
    return ranked"""),
    ],
}
