import unittest
from datetime import datetime

from rota.conflicts import find_overlaps, rest_violations
from rota.models import Shift


def shift(sid, start, end, emp="E1"):
    return Shift(sid, datetime(*start), datetime(*end), "nurse", employee=emp)


class ConflictTest(unittest.TestCase):

    def test_overlap_detected(self):
        shifts = [shift("S1", (2026, 3, 2, 6), (2026, 3, 2, 14)),
                  shift("S2", (2026, 3, 2, 12), (2026, 3, 2, 20))]
        found = find_overlaps(shifts)
        self.assertEqual([(c.kind, c.first, c.second) for c in found], [("overlap", "S1", "S2")])

    def test_rest_measured_from_end_of_shift(self):
        # Early shift then a night shift the same evening: only 8 hours of rest.
        shifts = [shift("S1", (2026, 3, 2, 6), (2026, 3, 2, 14)),
                  shift("S2", (2026, 3, 2, 22), (2026, 3, 3, 6))]
        found = rest_violations(shifts)
        self.assertEqual(len(found), 1)
        self.assertEqual((found[0].kind, found[0].first, found[0].second), ("rest", "S1", "S2"))

    def test_enough_rest(self):
        shifts = [shift("S1", (2026, 3, 2, 6), (2026, 3, 2, 14)),
                  shift("S2", (2026, 3, 3, 6), (2026, 3, 3, 14))]
        self.assertEqual(rest_violations(shifts), [])


if __name__ == "__main__":
    unittest.main()
