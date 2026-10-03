"""Ordering the league table.

Teams are ordered by:

1. points, descending;
2. goal difference, descending;
3. goals scored, descending;
4. head-to-head: among the teams still level after 1-3, the points earned
   in the matches between those teams only, descending; then the goal
   difference in those matches, descending;
5. team name, ascending.

The head-to-head step only looks at the group of teams that are level on
1-3 (two or more teams), and is applied once to that whole group.
"""

from .table import Rules


def head_to_head(group, matches, rules=None):
    """Dict team -> (points, goal difference) in matches among `group`."""
    rules = rules or Rules()
    members = set(group)
    stats = {team: [0, 0] for team in group}
    for match in matches:
        if match.home in members and match.away in members:
            stats[match.home][0] += rules.points(match.home_goals, match.away_goals)
            stats[match.away][0] += rules.points(match.away_goals, match.home_goals)
            stats[match.home][1] += match.home_goals - match.away_goals
            stats[match.away][1] += match.away_goals - match.home_goals
    return {team: tuple(values) for team, values in stats.items()}


def _primary_key(row):
    return (-row.points, -row.goal_diff, -row.goals_for)


def level_groups(rows):
    """Split rows (any order) into lists of teams level on points, goal
    difference and goals scored, best group first; each group keeps the
    order of `rows`."""
    groups = {}
    for row in rows:
        groups.setdefault(_primary_key(row), []).append(row)
    return [groups[key] for key in sorted(groups)]


def rank(rows, matches, rules=None):
    """The rows (a dict team -> Row or a list of Row) in table order."""
    if isinstance(rows, dict):
        rows = list(rows.values())
    return sorted(rows, key=lambda row: _primary_key(row) + (row.team,))


def positions(ranked):
    """(position, row) pairs, positions starting at 1."""
    return list(enumerate(ranked, 1))
