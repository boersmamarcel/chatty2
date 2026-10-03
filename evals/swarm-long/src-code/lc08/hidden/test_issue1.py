import unittest

from splitbill.expenses import parse_split
from splitbill.money import MoneyError, split_even


class SplitEvenTest(unittest.TestCase):
    def test_examples(self):
        self.assertEqual(split_even(1000, 3), [334, 333, 333])
        self.assertEqual(split_even(101, 4), [26, 25, 25, 25])
        self.assertEqual(split_even(2, 5), [1, 1, 0, 0, 0])
        self.assertEqual(split_even(900, 3), [300, 300, 300])
        self.assertEqual(split_even(7, 1), [7])
        self.assertEqual(split_even(0, 3), [0, 0, 0])

    def test_negative(self):
        self.assertEqual(split_even(-100, 3), [-34, -33, -33])
        self.assertEqual(split_even(-1, 2), [-1, 0])
        self.assertEqual(split_even(-1000, 3), [-334, -333, -333])
        self.assertEqual(split_even(-900, 3), [-300, -300, -300])

    def test_sums_and_types(self):
        for total in (-1001, -7, 0, 1, 99, 1000, 12345):
            for n in range(1, 8):
                shares = split_even(total, n)
                self.assertEqual(sum(shares), total)
                self.assertEqual(len(shares), n)
                self.assertTrue(all(isinstance(s, int) for s in shares))
                self.assertLessEqual(max(shares) - min(shares), 1)

    def test_bad_n(self):
        with self.assertRaises(MoneyError):
            split_even(100, 0)
        with self.assertRaises(MoneyError):
            split_even(100, -2)

    def test_equal_split_spec(self):
        shares = parse_split("equal:Anna, ben ,cleo", 1000)
        self.assertEqual(list(shares.items()), [("anna", 334), ("ben", 333), ("cleo", 333)])


if __name__ == "__main__":
    unittest.main()
