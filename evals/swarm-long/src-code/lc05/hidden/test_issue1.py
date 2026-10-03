import unittest

from throttle.clock import ManualClock
from throttle.plans import Rate
from throttle.token_bucket import TokenBucket


class Issue1CapacityTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(1000.0)

    def test_idle_refill_stops_at_capacity(self):
        bucket = TokenBucket(5, 0.5, self.clock)
        self.clock.advance(3600)
        self.assertEqual(bucket.available(), 5)
        self.assertLessEqual(bucket.remaining(), 5)

    def test_full_bucket_allows_exactly_capacity(self):
        bucket = TokenBucket(5, 1, self.clock)
        self.clock.advance(3600)
        results = [bucket.try_acquire() for _ in range(7)]
        self.assertEqual(results, [True] * 5 + [False, False])

    def test_partially_drained_then_idle(self):
        bucket = TokenBucket(4, 2, self.clock)
        self.assertTrue(bucket.try_acquire(3))
        self.clock.advance(100)
        self.assertEqual(bucket.available(), 4)
        self.assertEqual(bucket.reset_after(), 0.0)
        self.assertTrue(bucket.try_acquire(4))
        self.assertFalse(bucket.try_acquire())

    def test_refill_crossing_capacity_mid_way(self):
        bucket = TokenBucket(4, 1, self.clock, initial=3)
        self.clock.advance(2.5)
        self.assertEqual(bucket.available(), 4)
        self.assertTrue(bucket.try_acquire(4))
        self.clock.advance(1.25)
        self.assertEqual(bucket.available(), 1.25)

    def test_partial_refill_unchanged(self):
        bucket = TokenBucket(4, 0.5, self.clock)
        self.assertTrue(bucket.try_acquire(4))
        self.clock.advance(3)
        self.assertEqual(bucket.available(), 1.5)
        self.assertEqual(bucket.time_until(2), 1.0)
        self.assertFalse(bucket.try_acquire(2))
        self.assertEqual(bucket.available(), 1.5)

    def test_for_rate_burst_bucket_capped(self):
        bucket = TokenBucket.for_rate(Rate(10, 10), self.clock, burst=3)
        self.clock.advance(1000)
        self.assertEqual(bucket.available(), 3)


class Issue1CostTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(0.0)
        self.bucket = TokenBucket(5, 1, self.clock)

    def test_try_acquire_cost_above_capacity(self):
        with self.assertRaises(ValueError) as ctx:
            self.bucket.try_acquire(6)
        self.assertEqual(str(ctx.exception), "cost 6 exceeds bucket capacity 5")
        self.assertEqual(self.bucket.available(), 5)

    def test_can_acquire_cost_above_capacity(self):
        with self.assertRaises(ValueError) as ctx:
            self.bucket.can_acquire(7)
        self.assertEqual(str(ctx.exception), "cost 7 exceeds bucket capacity 5")

    def test_time_until_cost_above_capacity(self):
        with self.assertRaises(ValueError) as ctx:
            self.bucket.time_until(6)
        self.assertEqual(str(ctx.exception), "cost 6 exceeds bucket capacity 5")

    def test_fractional_cost_above_capacity(self):
        with self.assertRaises(ValueError) as ctx:
            self.bucket.try_acquire(5.5)
        self.assertEqual(str(ctx.exception), "cost 5.5 exceeds bucket capacity 5")

    def test_cost_equal_to_capacity_is_valid(self):
        self.assertTrue(self.bucket.can_acquire(5))
        self.assertTrue(self.bucket.try_acquire(5))
        self.assertFalse(self.bucket.can_acquire(5))
        self.assertEqual(self.bucket.time_until(5), 5.0)

    def test_non_positive_cost_still_rejected(self):
        for cost in (0, -1):
            with self.assertRaises(ValueError) as ctx:
                self.bucket.try_acquire(cost)
            self.assertEqual(str(ctx.exception), "cost must be positive")


if __name__ == "__main__":
    unittest.main()
