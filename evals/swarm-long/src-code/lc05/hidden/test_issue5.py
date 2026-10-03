import datetime
import unittest

from throttle.clock import ManualClock
from throttle.periods import (UTC, BillingPeriod, anchor_date, billing_period,
                              periods_between)
from throttle.quota import QuotaLedger


def dt(*args):
    return datetime.datetime(*args, tzinfo=UTC)


def ts(*args):
    return dt(*args).timestamp()


class Issue5AnchorDateTest(unittest.TestCase):
    def test_clamped_to_month_end(self):
        self.assertEqual(anchor_date(2027, 2, 31), dt(2027, 2, 28))
        self.assertEqual(anchor_date(2027, 2, 29), dt(2027, 2, 28))
        self.assertEqual(anchor_date(2028, 2, 30), dt(2028, 2, 29))
        self.assertEqual(anchor_date(2027, 4, 31), dt(2027, 4, 30))
        self.assertEqual(anchor_date(2027, 3, 31), dt(2027, 3, 31))

    def test_regular_days_unchanged(self):
        self.assertEqual(anchor_date(2027, 2, 1), dt(2027, 2, 1))
        self.assertEqual(anchor_date(2027, 2, 28), dt(2027, 2, 28))
        self.assertEqual(anchor_date(2027, 12, 15), dt(2027, 12, 15))
        self.assertEqual(anchor_date(2027, 2, 31).tzinfo, UTC)


class Issue5ContainsTest(unittest.TestCase):
    def test_half_open(self):
        period = BillingPeriod(dt(2027, 1, 1), dt(2027, 2, 1))
        self.assertTrue(period.contains(dt(2027, 1, 1)))
        self.assertTrue(period.contains(dt(2027, 1, 31, 23, 59, 59)))
        self.assertFalse(period.contains(dt(2027, 2, 1)))
        self.assertFalse(period.contains(dt(2026, 12, 31, 23, 59, 59)))


class Issue5BillingPeriodTest(unittest.TestCase):
    def test_anchor_31_examples(self):
        self.assertEqual(billing_period(31, ts(2027, 2, 28)),
                         BillingPeriod(dt(2027, 2, 28), dt(2027, 3, 31)))
        self.assertEqual(billing_period(31, ts(2027, 2, 27, 23, 59, 59)),
                         BillingPeriod(dt(2027, 1, 31), dt(2027, 2, 28)))
        self.assertEqual(billing_period(31, ts(2027, 3, 15)),
                         BillingPeriod(dt(2027, 2, 28), dt(2027, 3, 31)))
        self.assertEqual(billing_period(31, ts(2027, 4, 30, 6)),
                         BillingPeriod(dt(2027, 4, 30), dt(2027, 5, 31)))

    def test_leap_year_anchor_30(self):
        self.assertEqual(billing_period(30, ts(2028, 2, 10)),
                         BillingPeriod(dt(2028, 1, 30), dt(2028, 2, 29)))
        self.assertEqual(billing_period(30, ts(2028, 2, 29)),
                         BillingPeriod(dt(2028, 2, 29), dt(2028, 3, 30)))
        self.assertEqual(billing_period(30, ts(2028, 3, 29, 23, 59, 59)),
                         BillingPeriod(dt(2028, 2, 29), dt(2028, 3, 30)))

    def test_exact_start_belongs_to_new_period(self):
        self.assertEqual(billing_period(1, ts(2027, 6, 1)),
                         BillingPeriod(dt(2027, 6, 1), dt(2027, 7, 1)))
        self.assertEqual(billing_period(15, ts(2027, 6, 15)),
                         BillingPeriod(dt(2027, 6, 15), dt(2027, 7, 15)))

    def test_year_boundary(self):
        self.assertEqual(billing_period(31, ts(2027, 1, 15)),
                         BillingPeriod(dt(2026, 12, 31), dt(2027, 1, 31)))
        self.assertEqual(billing_period(1, ts(2027, 1, 1)),
                         BillingPeriod(dt(2027, 1, 1), dt(2027, 2, 1)))

    def test_periods_between_anchor_31(self):
        got = periods_between(31, ts(2027, 1, 31), ts(2027, 5, 1))
        starts = [p.start for p in got]
        self.assertEqual(starts, [dt(2027, 1, 31), dt(2027, 2, 28), dt(2027, 3, 31), dt(2027, 4, 30)])


class Issue5QuotaRolloverTest(unittest.TestCase):
    def test_usage_resets_exactly_at_clamped_anchor(self):
        clock = ManualClock(ts(2027, 2, 27, 23, 59, 59))
        ledger = QuotaLedger(clock)
        ledger.charge("acme", 5, 100, anchor_day=31)
        self.assertEqual(ledger.usage("acme", 31), 5)
        clock.set(ts(2027, 2, 28))
        self.assertEqual(ledger.usage("acme", 31), 0)
        self.assertEqual(ledger.history("acme"),
                         [(BillingPeriod(dt(2027, 1, 31), dt(2027, 2, 28)), 5)])
        self.assertEqual(ledger.seconds_until_reset("acme", 31), 31 * 86400.0)


if __name__ == "__main__":
    unittest.main()
