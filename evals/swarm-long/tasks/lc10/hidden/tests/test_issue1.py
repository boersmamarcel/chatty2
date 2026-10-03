import unittest

from standings.results import ResultError, parse_results


class DigitsAndPostponedTest(unittest.TestCase):
    def test_digits(self):
        m = parse_results("2026-08-20  Schalke 04 2-1 FC 1912 Rovers\n")[0]
        self.assertEqual((m.home, m.home_goals, m.away_goals, m.away), ("Schalke 04", 2, 1, "FC 1912 Rovers"))
        m = parse_results("2026-08-20 1. FC Union 0-0 Hertha\n")[0]
        self.assertEqual((m.home, m.away), ("1. FC Union", "Hertha"))
        m = parse_results("2026-08-20 Hannover 96 10-0 Team 2\n")[0]
        self.assertEqual((m.home, m.home_goals, m.away_goals, m.away), ("Hannover 96", 10, 0, "Team 2"))
        m = parse_results("2026-08-20   FC  1912   Rovers   3-2   St. Mirren's  B\n")[0]
        self.assertEqual((m.home, m.away), ("FC 1912 Rovers", "St. Mirren's B"))

    def test_postponed(self):
        text = ("2026-08-14  Ajax 2-1 PSV\n"
                "2026-08-21  NAC Breda P-P Sparta Rotterdam\n"
                "2026-08-22  Schalke 04 p-p FC 1912 Rovers\n"
                "2026-08-23  PSV 0-3 Ajax\n")
        matches = parse_results(text)
        self.assertEqual([(m.home, m.away) for m in matches], [("Ajax", "PSV"), ("PSV", "Ajax")])

    def test_postponed_still_validated(self):
        for bad in ["2026-13-40 NAC Breda P-P Sparta\n", "2026-08-21 P-P Sparta\n",
                    "2026-08-21 NAC Breda P-P\n", "NAC Breda P-P Sparta\n"]:
            with self.assertRaises(ResultError, msg=bad):
                parse_results(bad)

    def test_malformed_line_number(self):
        text = "# header\n2026-08-14  Ajax 2-1 PSV\n\n2026-08-15 Ajax 2:1 PSV\n"
        with self.assertRaises(ResultError) as ctx:
            parse_results(text)
        self.assertTrue(str(ctx.exception).startswith("line 4: "), str(ctx.exception))
        with self.assertRaises(ResultError):
            parse_results("2026-08-15 Ajax 2-1 Ajax\n")


if __name__ == "__main__":
    unittest.main()
