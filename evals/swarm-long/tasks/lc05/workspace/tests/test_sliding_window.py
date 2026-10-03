import unittest

from throttle.clock import ManualClock
from throttle.sliding_window import SlidingWindowLog


class SlidingWindowTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(0.0)

    def test_limit_within_window(self):
        log = SlidingWindowLog(3, 60, self.clock)
        self.assertTrue(log.try_acquire())
        self.assertTrue(log.try_acquire())
        self.assertTrue(log.try_acquire())
        self.assertFalse(log.can_acquire())
        self.assertEqual(log.remaining(), 0)

    def test_entry_expires_exactly_one_window_later(self):
        log = SlidingWindowLog(1, 10, self.clock)
        self.assertTrue(log.try_acquire())
        self.clock.advance(9.5)
        self.assertEqual(log.count(), 1)
        self.clock.set(10.0)
        self.assertEqual(log.count(), 0)
        self.assertTrue(log.try_acquire())


if __name__ == "__main__":
    unittest.main()
