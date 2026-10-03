import unittest

from throttle.config import parse_config
from throttle.errors import ConfigError
from throttle.plans import Rate, parse_rate


class Issue4ParseRateTest(unittest.TestCase):
    def test_case_insensitive_units(self):
        self.assertEqual(parse_rate("100/Min"), Rate(100, 60))
        self.assertEqual(parse_rate("10/S"), Rate(10, 1))
        self.assertEqual(parse_rate("2/HOUR"), Rate(2, 3600))
        self.assertEqual(parse_rate("30/5M"), Rate(30, 300))
        self.assertEqual(parse_rate(" 7 / Days "), Rate(7, 86400))

    def test_per_form(self):
        self.assertEqual(parse_rate("30 per minute"), Rate(30, 60))
        self.assertEqual(parse_rate("30 per 5 minutes"), Rate(30, 300))
        self.assertEqual(parse_rate("7 PER day"), Rate(7, 86400))
        self.assertEqual(parse_rate("  12 Per 2 Hours  "), Rate(12, 7200))
        self.assertEqual(parse_rate("5 per 10s"), Rate(5, 10))

    def test_existing_forms_still_work(self):
        self.assertEqual(parse_rate("100/min"), Rate(100, 60))
        self.assertEqual(parse_rate("30/5m"), Rate(30, 300))

    def test_messages_unchanged(self):
        cases = [
            ("abc", "invalid rate 'abc'"),
            ("30 per", "invalid rate '30 per'"),
            ("30per minute", "invalid rate '30per minute'"),
            ("3/Fortnight", "unknown unit 'fortnight'"),
            ("3 per weeks", "unknown unit 'weeks'"),
            ("0/min", "rate limit must be positive"),
            ("5 per 0 minutes", "rate period must be positive"),
        ]
        for text, message in cases:
            with self.assertRaises(ConfigError) as ctx:
                parse_rate(text)
            self.assertEqual(ctx.exception.errors, [message], text)


EXAMPLE = """[plan free]
rate = 10/Second
rate = 5 per hours
burst = lots
[plan empty]
rate = 3/fortnight
[tenants]
acme = gold
"""


class Issue4ConfigErrorsTest(unittest.TestCase):
    def errors_of(self, text):
        with self.assertRaises(ConfigError) as ctx:
            parse_config(text)
        return ctx.exception.errors

    def test_issue_example(self):
        self.assertEqual(self.errors_of(EXAMPLE), [
            "line 4: burst must be an integer",
            "line 5: plan 'empty' has no rate",
            "line 6: unknown unit 'fortnight'",
            "line 8: tenant 'acme' refers to unknown plan 'gold'",
        ])

    def test_all_kinds_collected_in_line_order(self):
        text = "\n".join([
            "orphan = 1",                 # 1
            "[plan a]",                   # 2
            "rate = 1/s",                 # 3
            "color = blue",               # 4
            "algorithm = leaky",          # 5
            "",                           # 6
            "[plan a]",                   # 7
            "rate = 2/s",                 # 8
            "[weird section]",            # 9
            "just text",                  # 10
            "[plan b]",                   # 11
            "rate = nope",                # 12
            "[tenants]",                  # 13
            "x = a",                      # 14
        ])
        self.assertEqual(self.errors_of(text), [
            "line 1: setting outside of a section",
            "line 4: unknown key 'color'",
            "line 5: unknown algorithm 'leaky'",
            "line 7: duplicate plan 'a'",
            "line 9: unknown section 'weird section'",
            "line 10: expected 'key = value'",
            "line 11: plan 'b' has no rate",
            "line 12: invalid rate 'nope'",
        ])

    def test_plan_errors_on_header_line_sorted(self):
        text = "\n".join([
            "[plan late]",        # 1
            "quota = -5",         # 2
            "rate = 1/s",         # 3
            "rate = 1/eon",       # 4
        ])
        self.assertEqual(self.errors_of(text), [
            "line 1: quota must not be negative",
            "line 4: unknown unit 'eon'",
        ])

    def test_single_error_still_reported(self):
        self.assertEqual(self.errors_of("[plan x]\nrate = 1/s\nburst = 0\n"),
                         ["line 1: burst must be positive"])

    def test_valid_file_with_new_rate_forms(self):
        config = parse_config("\n".join([
            "# plans",
            "[plan free]",
            "rate = 10/Second",
            "rate = 100 per Hour",
            "burst = 20",
            "[plan pro]",
            "rate = 30 per 5 minutes",
            "algorithm = sliding_window",
            "quota = 1000",
            "anchor_day = 15",
            "[tenants]",
            " Acme = pro",
            "initech = free",
        ]))
        self.assertEqual(sorted(config.plans), ["free", "pro"])
        self.assertEqual(config.plans["free"].rates, [Rate(10, 1), Rate(100, 3600)])
        self.assertEqual(config.plans["free"].burst, 20)
        self.assertEqual(config.plans["pro"].rates, [Rate(30, 300)])
        self.assertEqual(config.plans["pro"].algorithm, "sliding_window")
        self.assertEqual(config.plans["pro"].quota, 1000)
        self.assertEqual(config.plans["pro"].anchor_day, 15)
        self.assertEqual(config.tenants, {"acme": "pro", "initech": "free"})
        self.assertIs(config.plan_for("ACME"), config.plans["pro"])


if __name__ == "__main__":
    unittest.main()
