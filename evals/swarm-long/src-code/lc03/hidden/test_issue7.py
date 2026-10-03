import unittest
from datetime import datetime

from rota.availability import common_windows, find_slot
from rota.intervals import Interval


def t(h, m=0):
    return datetime(2026, 3, 4, h, m)


def iv(h1, m1, h2, m2):
    return Interval(t(h1, m1), t(h2, m2))


class SlotTest(unittest.TestCase):

    def test_touching_windows(self):
        windows = {"alice": [iv(9, 0, 10, 0), iv(10, 0, 11, 30)], "bob": [iv(9, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 90), iv(9, 0, 10, 30))

    def test_overlapping_windows_of_one_person(self):
        windows = {"alice": [iv(9, 0, 10, 30), iv(10, 0, 11, 30)], "bob": [iv(9, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 120), iv(9, 0, 11, 0))

    def test_unsorted_windows(self):
        windows = {"alice": [iv(13, 0, 15, 0), iv(9, 0, 9, 30), iv(9, 30, 10, 0)],
                   "bob": [iv(8, 0, 17, 0)]}
        self.assertEqual(find_slot(windows, 60), iv(9, 0, 10, 0))

    def test_common_windows_combined(self):
        windows = {"alice": [iv(10, 0, 11, 0), iv(9, 0, 10, 0)], "bob": [iv(8, 0, 17, 0)]}
        self.assertEqual(common_windows(windows), [iv(9, 0, 11, 0)])

    def test_exact_fit(self):
        windows = {"alice": [iv(9, 0, 10, 0)], "bob": [iv(8, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 60), iv(9, 0, 10, 0))

    def test_exact_fit_to_latest(self):
        windows = {"alice": [iv(9, 0, 12, 0)], "bob": [iv(9, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 60, earliest=t(10), latest=t(11)), iv(10, 0, 11, 0))
        self.assertIsNone(find_slot(windows, 60, earliest=t(10), latest=t(10, 59)))

    def test_alignment(self):
        windows = {"alice": [iv(9, 10, 11, 0)], "bob": [iv(9, 0, 12, 0)]}
        self.assertEqual(find_slot(windows, 30), iv(9, 15, 9, 45))
        self.assertEqual(find_slot(windows, 30, earliest=t(9, 20)), iv(9, 30, 10, 0))
        self.assertEqual(find_slot(windows, 30, granularity=30), iv(9, 30, 10, 0))

    def test_alignment_pushes_to_next_window(self):
        windows = {"alice": [iv(9, 5, 9, 50), iv(14, 0, 15, 0)], "bob": [iv(8, 0, 18, 0)]}
        self.assertEqual(find_slot(windows, 45), iv(14, 0, 14, 45))

    def test_three_people(self):
        windows = {"alice": [iv(9, 0, 10, 0), iv(10, 0, 13, 0)],
                   "bob": [iv(11, 0, 12, 0), iv(9, 30, 11, 0)],
                   "cara": [iv(10, 30, 12, 30)]}
        self.assertEqual(find_slot(windows, 90), iv(10, 30, 12, 0))


if __name__ == "__main__":
    unittest.main()
