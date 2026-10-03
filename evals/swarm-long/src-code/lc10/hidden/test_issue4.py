import datetime
import unittest

from standings.form import form, form_table, streak
from standings.results import Match


def day(n):
    return datetime.date(2026, 8, 1) + datetime.timedelta(days=n)


MATCHES = [
    Match(day(0), "Ajax", "PSV", 2, 0),      # Ajax W
    Match(day(7), "AZ", "Ajax", 1, 1),       # Ajax D
    Match(day(21), "Ajax", "Twente", 0, 1),  # Ajax L
    Match(day(28), "Ajax", "AZ", 3, 0),      # Ajax W
    Match(day(14), "PSV", "Ajax", 0, 1),     # Ajax W (late correction)
    Match(day(35), "Utrecht", "Ajax", 2, 2), # Ajax D
    Match(day(3), "PSV", "AZ", 1, 0),
]


class FormTest(unittest.TestCase):
    def test_date_order(self):
        # by date: W(0) D(7) W(14) L(21) W(28) D(35)
        self.assertEqual(form("Ajax", MATCHES, 5), "DWLWD")
        self.assertEqual(form("Ajax", MATCHES), "DWLWD")
        self.assertEqual(form("Ajax", MATCHES, 3), "LWD")
        self.assertEqual(form("Ajax", MATCHES, 10), "WDWLWD")

    def test_zero_and_negative(self):
        self.assertEqual(form("Ajax", MATCHES, 0), "")
        self.assertEqual(form("Ajax", MATCHES, -2), "")

    def test_other_teams(self):
        self.assertEqual(form("PSV", MATCHES), "LWL")
        self.assertEqual(form("Feyenoord", MATCHES), "")

    def test_same_day_keeps_list_order(self):
        matches = [Match(day(5), "A", "B", 1, 0), Match(day(1), "A", "C", 0, 0),
                   Match(day(5), "D", "A", 3, 0)]
        self.assertEqual(form("A", matches), "DWL")

    def test_derived(self):
        self.assertEqual(form_table(["Ajax", "PSV"], MATCHES, 2), {"Ajax": "WD", "PSV": "WL"})
        self.assertEqual(streak("Ajax", MATCHES), ("D", 1))
        self.assertEqual(streak("PSV", MATCHES[:5]), ("L", 2))


if __name__ == "__main__":
    unittest.main()
