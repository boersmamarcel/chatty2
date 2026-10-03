import collections
import unittest
from decimal import Decimal

from splitbill.expenses import SplitError, parse_split, split_percent


def od(*pairs):
    return collections.OrderedDict(pairs)


class SplitPercentTest(unittest.TestCase):
    def test_examples(self):
        got = split_percent(1000, od(("anna", Decimal("33.33")), ("ben", Decimal("33.33")),
                                     ("cleo", Decimal("33.34"))))
        self.assertEqual(list(got.items()), [("anna", 333), ("ben", 333), ("cleo", 334)])
        self.assertEqual(list(split_percent(1001, od(("a", 50), ("b", 50))).items()),
                         [("a", 501), ("b", 500)])
        self.assertEqual(list(split_percent(1, od(("a", 50), ("b", 50))).values()), [1, 0])
        self.assertEqual(list(split_percent(-1001, od(("a", 50), ("b", 50))).values()), [-501, -500])

    def test_names_and_types(self):
        got = split_percent(999, od((" Anna ", "60"), ("BEN", "40")))
        self.assertEqual(list(got.items()), [("anna", 599), ("ben", 400)])
        self.assertTrue(all(isinstance(v, int) for v in got.values()))

    def test_largest_remainder(self):
        got = split_percent(100, od(("a", Decimal("10.5")), ("b", Decimal("10.5")),
                                    ("c", Decimal("79"))))
        self.assertEqual(list(got.values()), [11, 10, 79])
        got = split_percent(7, od(("a", 20), ("b", 30), ("c", 50)))
        self.assertEqual(list(got.values()), [1, 2, 4])
        self.assertEqual(sum(got.values()), 7)

    def test_sum_always_total(self):
        for total in (1, 2, 3, 99, 100, 1001, 33333, -77):
            got = split_percent(total, od(("a", Decimal("33.33")), ("b", Decimal("33.33")),
                                          ("c", Decimal("33.34"))))
            self.assertEqual(sum(got.values()), total)

    def test_invalid(self):
        with self.assertRaises(SplitError):
            split_percent(1000, od(("a", 60), ("b", 30)))
        with self.assertRaises(SplitError):
            split_percent(1000, od(("a", 60), ("b", 50)))
        with self.assertRaises(SplitError):
            split_percent(1000, od(("a", 110), ("b", -10)))
        with self.assertRaises(SplitError):
            parse_split("percent:anna=60,ben=30", 1000)

    def test_via_spec(self):
        shares = parse_split("percent:anna=33.33,ben=33.33,cleo=33.34", 1000)
        self.assertEqual(list(shares.values()), [333, 333, 334])


if __name__ == "__main__":
    unittest.main()
