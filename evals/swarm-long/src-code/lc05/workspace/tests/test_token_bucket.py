import unittest

from throttle.clock import ManualClock
from throttle.plans import Rate
from throttle.token_bucket import TokenBucket


class TokenBucketTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(1000.0)

    def test_starts_full(self):
        bucket = TokenBucket(3, 1, self.clock)
        self.assertEqual(bucket.available(), 3)
        for _ in range(3):
            self.assertTrue(bucket.try_acquire())
        self.assertFalse(bucket.try_acquire())

    def test_refused_request_consumes_nothing(self):
        bucket = TokenBucket(2, 1, self.clock, initial=1)
        self.assertFalse(bucket.try_acquire(2))
        self.assertEqual(bucket.available(), 1)

    def test_for_rate(self):
        bucket = TokenBucket.for_rate(Rate(10, 20), self.clock)
        self.assertEqual(bucket.capacity, 10)
        self.assertEqual(bucket.refill_rate, 0.5)

    def test_idle_bucket_does_not_overflow(self):
        bucket = TokenBucket(5, 1, self.clock)
        self.clock.advance(3600)
        self.assertEqual(bucket.available(), 5)
        allowed = 0
        while bucket.try_acquire():
            allowed += 1
            if allowed > 100:
                break
        self.assertEqual(allowed, 5)


if __name__ == "__main__":
    unittest.main()
