"""Match results.

A results file has one match per line:

    2026-08-14  Ajax 2-1 PSV
    2026-08-15  Go Ahead Eagles 0-0 FC Twente
    2026-08-16  NAC Breda P-P Sparta Rotterdam

date (YYYY-MM-DD), home team, score, away team, separated by whitespace.
The score is `<home goals>-<away goals>`, or `P-P` for a postponed match
(which has not been played and is left out of the results). Team names
may contain letters, digits, spaces, dots, apostrophes and hyphens; runs of
whitespace inside a name count as one space. Blank lines and lines
starting with `#` are ignored.
"""

import datetime
import re


class ResultError(ValueError):
    """Raised for a malformed results line; the message names the line."""


class Match(object):
    """One played match."""

    __slots__ = ("date", "home", "away", "home_goals", "away_goals")

    def __init__(self, date, home, away, home_goals, away_goals):
        self.date = date
        self.home = home
        self.away = away
        self.home_goals = home_goals
        self.away_goals = away_goals

    def winner(self):
        """Name of the winning team, or None for a draw."""
        if self.home_goals > self.away_goals:
            return self.home
        if self.away_goals > self.home_goals:
            return self.away
        return None

    def involves(self, team):
        return team in (self.home, self.away)

    def __repr__(self):
        return "Match(%s %s %d-%d %s)" % (self.date, self.home, self.home_goals,
                                          self.away_goals, self.away)


_LINE_RE = re.compile(
    r"^(\d{4}-\d{2}-\d{2})\s+([A-Za-z0-9.' -]+?)\s+(?:(\d+)-(\d+)|([Pp]-[Pp]))"
    r"\s+([A-Za-z0-9.' -]+)$")


def _clean_name(name):
    return " ".join(name.split())


def parse_line(line):
    """Match of one results line (None for a postponed match); raises
    ValueError when malformed."""
    match = _LINE_RE.match(line.strip())
    if not match:
        raise ValueError("expected '<date> <home> <h>-<a> <away>'")
    date_text, home, home_goals, away_goals, postponed, away = match.groups()
    date = datetime.datetime.strptime(date_text, "%Y-%m-%d").date()
    home, away = _clean_name(home), _clean_name(away)
    if home == away:
        raise ValueError("a team cannot play itself: %r" % (home,))
    if postponed:
        return None
    return Match(date, home, away, int(home_goals), int(away_goals))


def parse_results(text):
    """Matches of a results file, in file order."""
    matches = []
    for line_no, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            match = parse_line(line)
        except ValueError as exc:
            raise ResultError("line %d: %s" % (line_no, exc))
        if match is not None:
            matches.append(match)
    return matches


def teams(matches):
    """Sorted names of every team in `matches`."""
    names = set()
    for match in matches:
        names.add(match.home)
        names.add(match.away)
    return sorted(names)


def between(matches, first, second):
    """The matches played between two teams, either way round, in order."""
    pair = {first, second}
    return [m for m in matches if {m.home, m.away} == pair]


def on_or_before(matches, date):
    """The matches played on or before `date` (a datetime.date): the
    results as they stood on that day."""
    return [m for m in matches if m.date <= date]


def goals(matches):
    """Total number of goals scored in `matches`."""
    return sum(m.home_goals + m.away_goals for m in matches)
