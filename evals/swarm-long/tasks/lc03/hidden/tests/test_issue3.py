import unittest
from datetime import datetime

from rota.conflicts import find_conflicts, rest_violations
from rota.models import Shift


def shift(sid, start, end, emp="E1"):
    return Shift(sid, datetime(*start), datetime(*end), "nurse", employee=emp)


S1 = ((2026, 3, 2, 6), (2026, 3, 2, 14))


class RestTest(unittest.TestCase):

    def test_message_uses_end_to_start_gap(self):
        found = rest_violations([shift("S1", *S1), shift("S2", (2026, 3, 2, 22), (2026, 3, 3, 6))])
        self.assertEqual([c.message for c in found],
                         ["E1: only 8h00m rest between S1 and S2 (minimum 11h00m)"])

    def test_exact_minimum_is_allowed(self):
        found = rest_violations([shift("S1", *S1), shift("S2", (2026, 3, 3, 1), (2026, 3, 3, 9))])
        self.assertEqual(found, [])

    def test_one_minute_short(self):
        found = rest_violations([shift("S1", *S1), shift("S2", (2026, 3, 3, 0, 59), (2026, 3, 3, 9))])
        self.assertEqual([c.message for c in found],
                         ["E1: only 10h59m rest between S1 and S2 (minimum 11h00m)"])

    def test_unsorted_input(self):
        found = rest_violations([shift("S2", (2026, 3, 2, 22), (2026, 3, 3, 6)), shift("S1", *S1)])
        self.assertEqual([(c.first, c.second) for c in found], [("S1", "S2")])
        self.assertEqual(found[0].message, "E1: only 8h00m rest between S1 and S2 (minimum 11h00m)")

    def test_unsorted_with_enough_rest(self):
        found = rest_violations([shift("S2", (2026, 3, 3, 6), (2026, 3, 3, 14)), shift("S1", *S1)])
        self.assertEqual(found, [])

    def test_custom_minimum(self):
        pair = [shift("S1", *S1), shift("S2", (2026, 3, 2, 22), (2026, 3, 3, 6))]
        self.assertEqual(rest_violations(pair, min_rest_hours=8), [])
        found = rest_violations(pair, min_rest_hours=9)
        self.assertEqual([c.message for c in found],
                         ["E1: only 8h00m rest between S1 and S2 (minimum 9h00m)"])

    def test_chain_of_three_unsorted(self):
        shifts = [shift("C", (2026, 3, 3, 14), (2026, 3, 3, 22)),
                  shift("A", (2026, 3, 2, 14), (2026, 3, 2, 22)),
                  shift("B", (2026, 3, 3, 6), (2026, 3, 3, 12))]
        found = rest_violations(shifts)
        self.assertEqual([c.message for c in found], [
            "E1: only 8h00m rest between A and B (minimum 11h00m)",
            "E1: only 2h00m rest between B and C (minimum 11h00m)",
        ])

    def test_per_employee_and_unassigned(self):
        shifts = [shift("S1", *S1, emp="E2"),
                  shift("S3", (2026, 3, 2, 22), (2026, 3, 3, 6), emp="E1"),
                  shift("S2", (2026, 3, 2, 22), (2026, 3, 3, 6), emp="E2"),
                  shift("S4", (2026, 3, 3, 10), (2026, 3, 3, 12), emp=None),
                  shift("S5", (2026, 3, 3, 18), (2026, 3, 3, 22), emp="E1")]
        found = rest_violations(shifts)
        self.assertEqual([c.message for c in found],
                         ["E2: only 8h00m rest between S1 and S2 (minimum 11h00m)"])

    def test_overlap_not_reported_as_rest(self):
        shifts = [shift("S1", *S1), shift("S2", (2026, 3, 2, 13), (2026, 3, 2, 20))]
        self.assertEqual(rest_violations(shifts), [])
        self.assertEqual([c.kind for c in find_conflicts(shifts)], ["overlap"])


if __name__ == "__main__":
    unittest.main()
