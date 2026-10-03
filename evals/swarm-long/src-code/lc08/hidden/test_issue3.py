import collections
import unittest

from splitbill.settle import SettleError, settle


class SettleTest(unittest.TestCase):
    def test_example(self):
        balances = {"anna": 5000, "ben": 1000, "cleo": -3000, "dirk": -3000}
        self.assertEqual(settle(balances), [
            ("cleo", "anna", 3000),
            ("dirk", "anna", 2000),
            ("dirk", "ben", 1000),
        ])

    def test_reranked_after_partial_payment(self):
        balances = collections.OrderedDict([("a", -700), ("b", -500), ("c", 400), ("d", 800)])
        self.assertEqual(settle(balances), [
            ("a", "d", 700),
            ("b", "c", 400),
            ("b", "d", 100),
        ])

    def test_creditor_ties_by_name(self):
        balances = {"zed": 300, "amy": 300, "bob": -600}
        self.assertEqual(settle(balances), [("bob", "amy", 300), ("bob", "zed", 300)])

    def test_zero_balances_skipped(self):
        balances = {"a": 0, "b": 250, "c": -250, "d": 0}
        self.assertEqual(settle(balances), [("c", "b", 250)])
        self.assertEqual(settle({"a": 0, "b": 0}), [])
        self.assertEqual(settle({}), [])

    def test_not_zero_sum(self):
        with self.assertRaises(SettleError):
            settle({"a": 100, "b": -90})
        with self.assertRaises(SettleError):
            settle({"a": -5})


if __name__ == "__main__":
    unittest.main()
