"""Reference fixes for lc10 (standings): exact replacements per issue."""

FIXES = {
    1: [
        ("standings/results.py",
         """    r"^(\\d{4}-\\d{2}-\\d{2})\\s+([A-Za-z.' -]+?)\\s+(\\d+)-(\\d+)\\s+([A-Za-z.' -]+)$")""",
         """    r"^(\\d{4}-\\d{2}-\\d{2})\\s+([A-Za-z0-9.' -]+?)\\s+(?:(\\d+)-(\\d+)|([Pp]-[Pp]))"
    r"\\s+([A-Za-z0-9.' -]+)$")"""),
        ("standings/results.py",
         """    \"\"\"Match of one results line; raises ValueError when malformed.\"\"\"""",
         """    \"\"\"Match of one results line (None for a postponed match); raises
    ValueError when malformed.\"\"\""""),
        ("standings/results.py",
         """    date_text, home, home_goals, away_goals, away = match.groups()
    date = datetime.datetime.strptime(date_text, "%Y-%m-%d").date()
    home, away = _clean_name(home), _clean_name(away)
    if home == away:
        raise ValueError("a team cannot play itself: %r" % (home,))
    return Match""",
         """    date_text, home, home_goals, away_goals, postponed, away = match.groups()
    date = datetime.datetime.strptime(date_text, "%Y-%m-%d").date()
    home, away = _clean_name(home), _clean_name(away)
    if home == away:
        raise ValueError("a team cannot play itself: %r" % (home,))
    if postponed:
        return None
    return Match"""),
        ("standings/results.py",
         """        try:
            matches.append(parse_line(line))
        except ValueError as exc:
            raise ResultError("line %d: %s" % (line_no, exc))""",
         """        try:
            match = parse_line(line)
        except ValueError as exc:
            raise ResultError("line %d: %s" % (line_no, exc))
        if match is not None:
            matches.append(match)"""),
    ],
    2: [
        ("standings/tiebreak.py",
         """    if isinstance(rows, dict):
        rows = list(rows.values())
    return sorted(rows, key=lambda row: _primary_key(row) + (row.team,))""",
         """    if isinstance(rows, dict):
        rows = list(rows.values())
    ranked = []
    for group in level_groups(rows):
        if len(group) == 1:
            ranked.extend(group)
            continue
        h2h = head_to_head([row.team for row in group], matches, rules)
        ranked.extend(sorted(group, key=lambda row: (-h2h[row.team][0], -h2h[row.team][1],
                                                     row.team)))
    return ranked"""),
    ],
    3: [
        ("standings/table.py",
         """        home.add(match.home_goals, match.away_goals)
        away.add(match.away_goals, match.home_goals)
    return rows""",
         """        home.add(match.home_goals, match.away_goals)
        away.add(match.away_goals, match.home_goals)
    for team, points in (deductions or {}).items():
        if not isinstance(points, int) or isinstance(points, bool) or points < 0:
            raise ValueError("deduction for %s must be a non-negative int: %r" % (team, points))
        rows.setdefault(team, Row(team, rules)).deducted = points
    return rows"""),
    ],
    4: [
        ("standings/form.py",
         """    played = [m for m in matches if m.involves(team)]
    recent = played[-n:]""",
         """    if n <= 0:
        return ""
    played = sorted((m for m in matches if m.involves(team)), key=lambda m: m.date)
    recent = played[-n:]"""),
    ],
    5: [
        ("standings/render.py",
         """    for position, row in enumerate(ranked, 1):
        lines.append(_line((position, row.team, row.played, row.won, row.drawn,
                            row.lost, row.goals_for, row.goals_against,
                            row.goal_diff, row.points)))
    return "\\n".join(lines)""",
         """    for position, row in enumerate(ranked, 1):
        team = row.team
        if len(team) > NAME_WIDTH:
            team = team[:NAME_WIDTH - 1] + "."
        goal_diff = "%+d" % row.goal_diff if row.goal_diff else "0"
        lines.append(_line((position, team, row.played, row.won, row.drawn,
                            row.lost, row.goals_for, row.goals_against,
                            goal_diff, row.points)))
    return "\\n".join(lines)"""),
    ],
}
