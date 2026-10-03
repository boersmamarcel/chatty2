import unittest

from throttle.clock import ManualClock
from throttle.plans import Rate
from throttle.sliding_window import SlidingWindowLog


class Issue3BoundaryTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(0.0)

    def test_entry_counts_until_just_before_expiry(self):
        log = SlidingWindowLog(2, 10, self.clock)
        log.try_acquire()
        self.clock.set(9.999)
        self.assertEqual(log.count(), 1)
        self.clock.set(10.0)
        self.assertEqual(log.count(), 0)
        self.assertEqual(log.remaining(), 2)

    def test_full_window_frees_exactly_at_boundary(self):
        log = SlidingWindowLog(2, 60, self.clock)
        self.clock.set(100.0)
        self.assertTrue(log.try_acquire())
        self.clock.set(130.0)
        self.assertTrue(log.try_acquire())
        self.clock.set(159.5)
        self.assertFalse(log.can_acquire())
        self.clock.set(160.0)
        self.assertTrue(log.can_acquire())
        self.assertTrue(log.try_acquire())
        self.assertFalse(log.try_acquire())
        self.clock.set(190.0)
        self.assertTrue(log.try_acquire())

    def test_reset_after_reaches_zero_at_boundary(self):
        log = SlidingWindowLog(3, 5, self.clock)
        log.try_acquire()
        self.clock.set(2.0)
        log.try_acquire()
        self.assertEqual(log.reset_after(), 5.0)
        self.clock.set(7.0)
        self.assertEqual(log.reset_after(), 0.0)
        self.assertEqual(log.count(), 0)

    def test_for_rate(self):
        log = SlidingWindowLog.for_rate(Rate(1, 60), self.clock)
        self.assertTrue(log.try_acquire())
        self.clock.set(60.0)
        self.assertTrue(log.try_acquire())


class Issue3RefusedTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(0.0)

    def test_refused_attempt_not_recorded(self):
        log = SlidingWindowLog(2, 10, self.clock)
        self.assertTrue(log.try_acquire())
        self.assertTrue(log.try_acquire())
        for _ in range(5):
            self.assertFalse(log.try_acquire())
        self.assertEqual(log.count(), 2)

    def test_refused_multi_cost_not_recorded(self):
        log = SlidingWindowLog(5, 10, self.clock)
        self.assertTrue(log.try_acquire(3))
        self.assertFalse(log.try_acquire(3))
        self.assertEqual(log.count(), 3)
        self.assertEqual(log.remaining(), 2)
        self.assertTrue(log.try_acquire(2))
        self.assertEqual(log.count(), 5)

    def test_hammering_client_gets_through_after_window(self):
        log = SlidingWindowLog(1, 10, self.clock)
        self.assertTrue(log.try_acquire())
        for step in range(1, 10):
            self.clock.set(float(step))
            self.assertFalse(log.try_acquire())
        self.clock.set(10.0)
        self.assertTrue(log.try_acquire())

    def test_retry_after_example(self):
        log = SlidingWindowLog(2, 10, self.clock)
        log.try_acquire()
        self.clock.set(4.0)
        log.try_acquire()
        self.clock.set(6.0)
        self.assertFalse(log.try_acquire())
        self.assertEqual(log.retry_after(), 4.0)
        self.clock.set(7.0)
        for _ in range(3):
            self.assertFalse(log.try_acquire())
        self.assertEqual(log.retry_after(), 3.0)
        self.assertEqual(log.retry_after(2), 7.0)

    def test_retry_after_zero_when_free(self):
        log = SlidingWindowLog(2, 10, self.clock)
        log.try_acquire()
        self.assertEqual(log.retry_after(), 0.0)


if __name__ == "__main__":
    unittest.main()
