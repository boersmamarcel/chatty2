import datetime
import unittest

from standings.results import Match
from standings.table import Rules, build_table

D = datetime.date(2026, 9, 1)
MATCHES = [Match(D, "Vitesse", "Ajax", 2, 0), Match(D, "PSV", "Vitesse", 1, 1)]


class DeductionTest(unittest.TestCase):
    def test_applied(self):
        rows = build_table(MATCHES, deductions={"Vitesse": 6, "Ajax": 1})
        self.assertEqual(rows["Vitesse"].points, -2)
        self.assertEqual(rows["Ajax"].points, -1)
        self.assertEqual(rows["PSV"].points, 1)
        self.assertEqual(rows["PSV"].deducted, 0)
        self.assertEqual(rows["Vitesse"].as_tuple()[-1], -2)

    def test_team_without_matches(self):
        rows = build_table(MATCHES, deductions={"Feyenoord": 3})
        self.assertIn("Feyenoord", rows)
        self.assertEqual(rows["Feyenoord"].played, 0)
        self.assertEqual(rows["Feyenoord"].points, -3)
        self.assertEqual(len(rows), 4)
        rows = build_table([], deductions={"Feyenoord": 0})
        self.assertEqual(rows["Feyenoord"].points, 0)

    def test_rules_still_used(self):
        rows = build_table(MATCHES, Rules(win=2), deductions={"Vitesse": 1})
        self.assertEqual(rows["Vitesse"].points, 2)

    def test_invalid(self):
        for bad in [-1, 1.5, "3", None]:
            with self.assertRaises(ValueError, msg=repr(bad)):
                build_table(MATCHES, deductions={"Vitesse": bad})

    def test_none_and_empty(self):
        self.assertEqual(build_table(MATCHES, deductions=None)["Vitesse"].points, 4)
        self.assertEqual(build_table(MATCHES, deductions={})["Vitesse"].points, 4)


if __name__ == "__main__":
    unittest.main()
