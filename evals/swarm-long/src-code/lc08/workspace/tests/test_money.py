import unittest

from splitbill.money import MoneyError, format_cents, parse_amount, split_even, split_weighted


class AmountTest(unittest.TestCase):
    def test_parse_and_format(self):
        self.assertEqual(parse_amount("12.5"), 1250)
        self.assertEqual(parse_amount(" -3.10 "), -310)
        self.assertEqual(format_cents(-310), "-3.10")
        with self.assertRaises(MoneyError):
            parse_amount("1.234")


class SplitEvenTest(unittest.TestCase):
    def test_exact(self):
        self.assertEqual(split_even(900, 3), [300, 300, 300])

    def test_extra_cents_go_to_the_first(self):
        self.assertEqual(split_even(1000, 3), [334, 333, 333])
        self.assertEqual(split_even(101, 4), [26, 25, 25, 25])

    def test_weighted(self):
        self.assertEqual(split_weighted(100, [1, 1, 1]), [34, 33, 33])


if __name__ == "__main__":
    unittest.main()
