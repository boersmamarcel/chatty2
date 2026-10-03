import datetime
import unittest

from standings.results import Match
from standings.table import build_table

D = datetime.date(2026, 9, 1)
MATCHES = [
    Match(D, "Vitesse", "Ajax", 2, 0),
    Match(D, "PSV", "Vitesse", 1, 1),
]


class BuildTableTest(unittest.TestCase):
    def test_rows(self):
        rows = build_table(MATCHES)
        self.assertEqual(rows["Vitesse"].as_tuple(), ("Vitesse", 2, 1, 1, 0, 3, 1, 2, 4))
        self.assertEqual(rows["Ajax"].points, 0)

    def test_deduction(self):
        rows = build_table(MATCHES, deductions={"Vitesse": 6})
        self.assertEqual(rows["Vitesse"].deducted, 6)
        self.assertEqual(rows["Vitesse"].points, -2)
        self.assertEqual(rows["PSV"].points, 1)


if __name__ == "__main__":
    unittest.main()
