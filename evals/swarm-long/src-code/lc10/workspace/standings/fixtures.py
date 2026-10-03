"""Fixture lists and what is still possible this season.

`round_robin(teams)` builds a double round-robin schedule with the circle
method: every team meets every other team twice, once at home and once
away; with an odd number of teams one team rests each round (a "bye").

`remaining(schedule, matches)` lists the scheduled pairings that have not
been played yet, and `max_points` / `can_still_win` answer the usual
end-of-season questions from a table and the remaining fixtures.
"""

from .table import Rules

BYE = None


def round_robin(teams):
    """List of rounds; each round is a list of (home, away) pairs.

    The first half of the season is the single round-robin of the circle
    method, the second half repeats it with home and away swapped.
    """
    names = list(teams)
    if len(names) < 2:
        return []
    if len(names) % 2:
        names.append(BYE)
    count = len(names)
    first_half = []
    order = names[:]
    for round_no in range(count - 1):
        pairs = []
        for i in range(count // 2):
            home, away = order[i], order[count - 1 - i]
            if round_no % 2 == 1 and i == 0:
                home, away = away, home
            if home is not BYE and away is not BYE:
                pairs.append((home, away))
        first_half.append(pairs)
        order = [order[0]] + [order[-1]] + order[1:-1]
    second_half = [[(away, home) for home, away in pairs] for pairs in first_half]
    return first_half + second_half


def remaining(schedule, matches):
    """Scheduled (home, away) pairings without a played match, in order."""
    played = {}
    for match in matches:
        key = (match.home, match.away)
        played[key] = played.get(key, 0) + 1
    left = []
    for pairs in schedule:
        for pair in pairs:
            if played.get(pair, 0):
                played[pair] -= 1
            else:
                left.append(pair)
    return left


def games_left(team, fixtures):
    """Number of remaining fixtures involving `team`."""
    return sum(1 for home, away in fixtures if team in (home, away))


def max_points(row, fixtures, rules=None):
    """Points `row`'s team ends with if it wins every remaining fixture."""
    rules = rules or Rules()
    return row.points + games_left(row.team, fixtures) * rules.win


def can_still_win(team, rows, fixtures, rules=None):
    """True if `team` can still reach the points of the current leader.

    A rough check used for the 'still in the race' marker: it ignores that
    rivals also play each other.
    """
    rows = rows if isinstance(rows, dict) else {row.team: row for row in rows}
    leader_points = max(row.points for row in rows.values())
    return max_points(rows[team], fixtures, rules) >= leader_points
