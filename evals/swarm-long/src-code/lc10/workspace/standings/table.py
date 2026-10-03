"""Per-team league table rows.

Points per match come from `Rules` (default: win 3, draw 1, loss 0).
A competition may also deduct points from a team (for example for a breach
of financial rules); deductions are given per team as a positive number of
points and are subtracted from the team's points. A team with a deduction
appears in the table even before it has played.
"""


class Rules(object):
    """Points for a win, a draw and a loss."""

    def __init__(self, win=3, draw=1, loss=0):
        self.win = win
        self.draw = draw
        self.loss = loss

    def points(self, goals_for, goals_against):
        if goals_for > goals_against:
            return self.win
        if goals_for == goals_against:
            return self.draw
        return self.loss


class Row(object):
    """One team's line in the table."""

    def __init__(self, team, rules=None):
        self.team = team
        self.rules = rules or Rules()
        self.played = 0
        self.won = 0
        self.drawn = 0
        self.lost = 0
        self.goals_for = 0
        self.goals_against = 0
        self.deducted = 0

    @property
    def goal_diff(self):
        return self.goals_for - self.goals_against

    @property
    def points(self):
        rules = self.rules
        return (self.won * rules.win + self.drawn * rules.draw + self.lost * rules.loss
                - self.deducted)

    def add(self, goals_for, goals_against):
        """Book one played match from this team's point of view."""
        self.played += 1
        self.goals_for += goals_for
        self.goals_against += goals_against
        if goals_for > goals_against:
            self.won += 1
        elif goals_for == goals_against:
            self.drawn += 1
        else:
            self.lost += 1

    def as_tuple(self):
        return (self.team, self.played, self.won, self.drawn, self.lost,
                self.goals_for, self.goals_against, self.goal_diff, self.points)

    def __repr__(self):
        return "Row%r" % (self.as_tuple(),)


def build_table(matches, rules=None, deductions=None):
    """Dict team -> Row for the played `matches`.

    `deductions` maps team -> points to deduct (a non-negative int).
    """
    rules = rules or Rules()
    rows = {}
    for match in matches:
        home = rows.setdefault(match.home, Row(match.home, rules))
        away = rows.setdefault(match.away, Row(match.away, rules))
        home.add(match.home_goals, match.away_goals)
        away.add(match.away_goals, match.home_goals)
    return rows


def points_per_game(row):
    """Average points per played match, rounded to 2 decimals (0.0 if none)."""
    if row.played == 0:
        return 0.0
    return round(row.points / row.played, 2)


def home_away_split(team, matches, rules=None):
    """(home Row, away Row) of one team: its home and away matches booked
    separately. Deductions are not included in either row.
    """
    rules = rules or Rules()
    home = Row(team, rules)
    away = Row(team, rules)
    for match in matches:
        if match.home == team:
            home.add(match.home_goals, match.away_goals)
        elif match.away == team:
            away.add(match.away_goals, match.home_goals)
    return home, away
