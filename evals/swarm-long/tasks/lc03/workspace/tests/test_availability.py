import unittest
from datetime import datetime

from rota.availability import find_slot
from rota.intervals import Interval


def iv(h1, m1, h2, m2, day=4):
    return Interval(datetime(2026, 3, day, h1, m1), datetime(2026, 3, day, h2, m2))


class AvailabilityTest(unittest.TestCase):

    def test_simple_slot(self):
        windows = {"alice": [iv(9, 0, 12, 0)], "bob": [iv(10, 0, 16, 0)]}
        self.assertEqual(find_slot(windows, 60), iv(10, 0, 11, 0))

    def test_touching_windows_form_one_free_period(self):
        windows = {"alice": [iv(9, 0, 10, 0), iv(10, 0, 11, 30)],
                   "bob": [iv(9, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 90), iv(9, 0, 10, 30))

    def test_no_slot(self):
        windows = {"alice": [iv(9, 0, 10, 0)], "bob": [iv(10, 0, 11, 0)]}
        self.assertIsNone(find_slot(windows, 30))


if __name__ == "__main__":
    unittest.main()
