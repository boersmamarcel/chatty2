import datetime
import unittest

from standings.results import Match
from standings.table import Rules, build_table
from standings.tiebreak import rank


def day(n):
    return datetime.date(2026, 9, 1) + datetime.timedelta(days=n)


def M(n, home, hg, ag, away):
    return Match(day(n), home, away, hg, ag)


def order(matches, rules=None):
    return [row.team for row in rank(build_table(matches, rules), matches, rules)]


class HeadToHeadTest(unittest.TestCase):
    def test_two_teams(self):
        matches = [M(0, "Beta", 2, 1, "Alpha"), M(7, "Gamma", 2, 1, "Beta"),
                   M(14, "Alpha", 2, 1, "Gamma"), M(21, "Gamma", 0, 0, "Delta")]
        self.assertEqual(order(matches), ["Gamma", "Beta", "Alpha", "Delta"])

    def test_three_teams_goal_difference(self):
        matches = [
            M(0, "X", 3, 0, "Y"), M(1, "Y", 1, 0, "Z"), M(2, "Z", 1, 0, "X"),
            M(3, "X", 1, 0, "W1"), M(4, "W2", 3, 0, "X"),
            M(5, "Y", 3, 0, "W1"), M(6, "W2", 1, 0, "Y"),
            M(7, "Z", 2, 0, "W1"), M(8, "W2", 3, 1, "Z"),
        ]
        self.assertEqual(order(matches), ["W2", "X", "Z", "Y", "W1"])

    def test_cycle_falls_back_to_name(self):
        matches = [M(0, "Cee", 1, 0, "Bee"), M(1, "Bee", 1, 0, "Ay"), M(2, "Ay", 1, 0, "Cee")]
        self.assertEqual(order(matches), ["Ay", "Bee", "Cee"])

    def test_rules_used(self):
        rules = Rules(win=2, draw=1, loss=0)
        matches = [M(0, "Beta", 2, 1, "Alpha"), M(7, "Gamma", 2, 1, "Beta"),
                   M(14, "Alpha", 2, 1, "Gamma"), M(21, "Gamma", 0, 0, "Delta")]
        self.assertEqual(order(matches, rules), ["Gamma", "Beta", "Alpha", "Delta"])

    def test_list_input(self):
        matches = [M(0, "Beta", 2, 1, "Alpha"), M(7, "Gamma", 2, 1, "Beta"),
                   M(14, "Alpha", 2, 1, "Gamma"), M(21, "Gamma", 0, 0, "Delta")]
        rows = list(build_table(matches).values())
        self.assertEqual([r.team for r in rank(rows, matches)], ["Gamma", "Beta", "Alpha", "Delta"])


if __name__ == "__main__":
    unittest.main()
