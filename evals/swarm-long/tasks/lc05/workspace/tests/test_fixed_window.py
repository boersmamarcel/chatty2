import unittest

from throttle.clock import ManualClock
from throttle.fixed_window import FixedWindowCounter


class FixedWindowTest(unittest.TestCase):
    def test_counter_resets_each_window(self):
        clock = ManualClock(120.0)
        counter = FixedWindowCounter(2, 60, clock)
        self.assertTrue(counter.try_acquire())
        self.assertTrue(counter.try_acquire())
        self.assertFalse(counter.try_acquire())
        self.assertEqual(counter.retry_after(), 60.0)
        clock.set(179.0)
        self.assertFalse(counter.try_acquire())
        self.assertEqual(counter.retry_after(), 1.0)
        clock.set(180.0)
        self.assertTrue(counter.try_acquire())
        self.assertEqual(counter.window_start(), 180)


if __name__ == "__main__":
    unittest.main()
