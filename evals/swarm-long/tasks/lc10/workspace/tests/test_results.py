import datetime
import unittest

from standings.results import ResultError, parse_results


class ParseResultsTest(unittest.TestCase):
    def test_simple(self):
        matches = parse_results("# round 1\n2026-08-14  Ajax 2-1 PSV\n\n2026-08-15 Go  Ahead Eagles 0-0 FC Twente\n")
        self.assertEqual(len(matches), 2)
        self.assertEqual(matches[0].date, datetime.date(2026, 8, 14))
        self.assertEqual((matches[0].home, matches[0].home_goals, matches[0].away_goals, matches[0].away),
                         ("Ajax", 2, 1, "PSV"))
        self.assertEqual(matches[1].home, "Go Ahead Eagles")

    def test_digits_in_names(self):
        matches = parse_results("2026-08-20  Schalke 04 2-1 FC 1912 Rovers\n")
        self.assertEqual(matches[0].home, "Schalke 04")
        self.assertEqual(matches[0].away, "FC 1912 Rovers")

    def test_postponed_skipped(self):
        text = "2026-08-14  Ajax 2-1 PSV\n2026-08-21  NAC Breda P-P Sparta Rotterdam\n"
        self.assertEqual([m.home for m in parse_results(text)], ["Ajax"])

    def test_malformed(self):
        with self.assertRaises(ResultError):
            parse_results("2026-08-14  Ajax two-one PSV\n")


if __name__ == "__main__":
    unittest.main()
