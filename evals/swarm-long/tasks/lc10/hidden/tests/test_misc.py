import datetime
import unittest

from standings.fixtures import remaining, round_robin
from standings.render import render_csv
from standings.results import Match
from standings.table import build_table
from standings.tiebreak import rank

D = datetime.date(2026, 9, 1)


class FixturesTest(unittest.TestCase):
    def test_round_robin(self):
        schedule = round_robin(["A", "B", "C", "D"])
        self.assertEqual(len(schedule), 6)
        pairs = [pair for rnd in schedule for pair in rnd]
        self.assertEqual(len(pairs), 12)
        self.assertEqual(len(set(pairs)), 12)
        odd = round_robin(["A", "B", "C"])
        self.assertEqual(sum(len(r) for r in odd), 6)

    def test_remaining(self):
        schedule = [[("A", "B")], [("B", "A")]]
        self.assertEqual(remaining(schedule, [Match(D, "A", "B", 1, 0)]), [("B", "A")])


class RankTest(unittest.TestCase):
    def test_points_then_goal_difference(self):
        matches = [Match(D, "A", "B", 3, 0), Match(D, "C", "D", 1, 0), Match(D, "B", "D", 0, 0)]
        ranked = rank(build_table(matches), matches)
        self.assertEqual([r.team for r in ranked], ["A", "C", "D", "B"])
        csv = render_csv(ranked)
        self.assertTrue(csv.startswith("pos,team,p,w,d,l,gf,ga,gd,pts\n1,A,1,1,0,0,3,0,3,3\n"))


if __name__ == "__main__":
    unittest.main()
