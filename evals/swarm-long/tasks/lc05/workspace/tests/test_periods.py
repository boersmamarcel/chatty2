import datetime
import unittest

from throttle.periods import UTC, BillingPeriod, billing_period


def ts(*args):
    return datetime.datetime(*args, tzinfo=UTC).timestamp()


def dt(*args):
    return datetime.datetime(*args, tzinfo=UTC)


class BillingPeriodTest(unittest.TestCase):
    def test_anchor_first_of_month(self):
        period = billing_period(1, ts(2027, 5, 17, 12, 0))
        self.assertEqual(period, BillingPeriod(dt(2027, 5, 1), dt(2027, 6, 1)))

    def test_anchor_mid_month(self):
        period = billing_period(15, ts(2027, 5, 3))
        self.assertEqual(period, BillingPeriod(dt(2027, 4, 15), dt(2027, 5, 15)))

    def test_anchor_31_in_february(self):
        period = billing_period(31, ts(2027, 2, 10))
        self.assertEqual(period, BillingPeriod(dt(2027, 1, 31), dt(2027, 2, 28)))


if __name__ == "__main__":
    unittest.main()
