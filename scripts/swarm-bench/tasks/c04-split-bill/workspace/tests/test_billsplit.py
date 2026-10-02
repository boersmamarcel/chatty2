import unittest

from billsplit import split


class SplitTest(unittest.TestCase):
    def test_even(self):
        self.assertEqual(split(900, 3), [300, 300, 300])

    def test_remainder_goes_first(self):
        self.assertEqual(split(100, 3), [34, 33, 33])
        self.assertEqual(split(101, 3), [34, 34, 33])

    def test_sum_is_preserved(self):
        for total in range(0, 500, 7):
            for people in range(1, 9):
                self.assertEqual(sum(split(total, people)), total)

    def test_no_people(self):
        with self.assertRaises(ValueError):
            split(100, 0)
