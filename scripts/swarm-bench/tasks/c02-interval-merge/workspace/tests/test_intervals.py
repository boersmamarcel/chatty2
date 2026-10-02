import unittest

from intervals import merge


class MergeTest(unittest.TestCase):
    def test_overlap(self):
        self.assertEqual(merge([(1, 4), (2, 6)]), [(1, 6)])

    def test_touching_intervals_merge(self):
        self.assertEqual(merge([(1, 3), (3, 5)]), [(1, 5)])

    def test_unsorted_input(self):
        self.assertEqual(merge([(8, 9), (1, 2), (2, 4)]), [(1, 4), (8, 9)])

    def test_disjoint(self):
        self.assertEqual(merge([(1, 2), (5, 6)]), [(1, 2), (5, 6)])

    def test_empty(self):
        self.assertEqual(merge([]), [])
