import unittest

from throttle import retry
from throttle.clock import ManualClock
from throttle.limiter import RateLimiter
from throttle.plans import Plan, Rate


class Issue2RetryAfterSecondsTest(unittest.TestCase):
    def test_rounds_up(self):
        cases = [(0.25, 1), (1.5, 2), (2.5, 3), (3.0, 3), (0.0001, 1), (7.01, 8), (0.5, 1)]
        for seconds, expected in cases:
            got = retry.retry_after_seconds(seconds)
            self.assertEqual(got, expected, seconds)
            self.assertIsInstance(got, int)

    def test_non_positive(self):
        self.assertEqual(retry.retry_after_seconds(0), 0)
        self.assertEqual(retry.retry_after_seconds(0.0), 0)
        self.assertEqual(retry.retry_after_seconds(-2.5), 0)


class Issue2HeadersTest(unittest.TestCase):
    def setUp(self):
        self.clock = ManualClock(1000.0)
        plan = Plan("p", [Rate(4, 8)], algorithm="token_bucket")
        self.limiter = RateLimiter({"p": plan}, self.clock, default_plan="p")

    def check(self, cost=1):
        return self.limiter.check("acme", "/v1/items", cost)

    def test_first_request_example(self):
        decision = self.check()
        self.assertTrue(decision.allowed)
        self.assertEqual(decision.headers(), {
            "X-RateLimit-Limit": "4",
            "X-RateLimit-Remaining": "3",
            "X-RateLimit-Reset": "2",
        })

    def test_fractional_remaining_rounds_down(self):
        self.check(3)
        self.clock.advance(3.75)  # 1 + 1.875 = 2.875 tokens
        decision = self.check(0.5)
        self.assertTrue(decision.allowed)
        headers = decision.headers()
        self.assertEqual(headers["X-RateLimit-Remaining"], "2")
        self.assertNotIn("Retry-After", headers)

    def test_half_token_left_is_zero(self):
        self.check(3)
        decision = self.check(0.5)
        self.assertTrue(decision.allowed)
        self.assertEqual(decision.headers()["X-RateLimit-Remaining"], "0")

    def test_refused_small_wait(self):
        self.check(4)
        self.clock.advance(1.75)  # 0.875 tokens, 0.125 missing -> 0.25 s
        decision = self.check()
        self.assertFalse(decision.allowed)
        self.assertEqual(decision.headers(), {
            "X-RateLimit-Limit": "4",
            "X-RateLimit-Remaining": "0",
            "X-RateLimit-Reset": "7",
            "Retry-After": "1",
        })

    def test_refused_half_second_wait(self):
        self.check(4)
        self.clock.advance(0.5)  # 0.25 tokens, need 2 -> 3.5 s
        decision = self.check(2)
        self.assertFalse(decision.allowed)
        self.assertEqual(decision.headers()["Retry-After"], "4")
        self.clock.advance(1.0)  # 0.75 tokens, need 2 -> 2.5 s
        decision = self.check(2)
        self.assertEqual(decision.headers()["Retry-After"], "3")
        self.assertEqual(decision.headers()["X-RateLimit-Remaining"], "0")

    def test_values_are_strings(self):
        self.check(4)
        for value in self.check().headers().values():
            self.assertIsInstance(value, str)
            self.assertTrue(value.isdigit(), value)

    def test_format_headers_unchanged_for_whole_numbers(self):
        headers = retry.format_headers(10, 3, 0.5, 2.5)
        self.assertEqual(headers, {
            "X-RateLimit-Limit": "10",
            "X-RateLimit-Remaining": "3",
            "X-RateLimit-Reset": "1",
            "Retry-After": "3",
        })


if __name__ == "__main__":
    unittest.main()
