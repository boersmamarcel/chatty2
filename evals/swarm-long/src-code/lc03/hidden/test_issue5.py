import unittest
from datetime import datetime, timedelta, timezone

from rota.ical import escape_text, export_calendar, fold_line, format_dt, unfold
from rota.models import Meeting

CEST = timezone(timedelta(hours=2))
EST = timezone(timedelta(hours=-5))


class EscapeTest(unittest.TestCase):

    def test_comma_semicolon(self):
        self.assertEqual(escape_text("Ward A, B; C"), "Ward A\\, B\; C")

    def test_backslash(self):
        self.assertEqual(escape_text("C:\\rota"), "C:\\\\rota")

    def test_mixed(self):
        self.assertEqual(escape_text("a\\b, c;d\ne"), "a\\\\b\\, c\;d\\ne")

    def test_crlf_newline(self):
        self.assertEqual(escape_text("x\r\ny"), "x\\ny")


class FoldTest(unittest.TestCase):

    def test_short_line_untouched(self):
        line = "X" * 75
        self.assertEqual(fold_line(line), line)

    def test_76_octets(self):
        line = "X" * 76
        self.assertEqual(fold_line(line), "X" * 75 + "\r\n " + "X")

    def test_ascii_long_line(self):
        line = "DESCRIPTION:" + "x" * 150
        expected = line[:75] + "\r\n " + line[75:149] + "\r\n " + line[149:]
        self.assertEqual(fold_line(line), expected)

    def test_multibyte_not_split(self):
        line = "SUMMARY:" + "\u00e9" * 40
        self.assertEqual(fold_line(line), "SUMMARY:" + "\u00e9" * 33 + "\r\n " + "\u00e9" * 7)

    def test_octet_limit_everywhere(self):
        line = "SUMMARY:" + "\u20ac\u00e9a" * 60
        folded = fold_line(line)
        for physical in folded.split("\r\n"):
            self.assertLessEqual(len(physical.encode("utf-8")), 75)
        self.assertEqual(unfold(folded), line)


class DateTimeTest(unittest.TestCase):

    def test_positive_offset(self):
        self.assertEqual(format_dt(datetime(2026, 3, 1, 9, 0, tzinfo=CEST)), "20260301T070000Z")

    def test_negative_offset_next_day(self):
        self.assertEqual(format_dt(datetime(2026, 3, 1, 22, 30, tzinfo=EST)), "20260302T033000Z")

    def test_utc(self):
        self.assertEqual(format_dt(datetime(2026, 3, 1, 9, 0, tzinfo=timezone.utc)),
                         "20260301T090000Z")

    def test_naive(self):
        self.assertEqual(format_dt(datetime(2026, 3, 1, 9, 0)), "20260301T090000")


class ExportTest(unittest.TestCase):

    def test_meeting_export(self):
        m = Meeting("M1", "Handover; ward 3, " + "night team " * 8,
                    datetime(2026, 3, 1, 9, 0, tzinfo=CEST),
                    datetime(2026, 3, 1, 9, 30, tzinfo=CEST))
        text = export_calendar([m], dtstamp=datetime(2026, 2, 28, 12, 0, tzinfo=timezone.utc))
        for physical in text.split("\r\n"):
            self.assertLessEqual(len(physical.encode("utf-8")), 75)
        flat = unfold(text)
        self.assertIn("\r\nDTSTART:20260301T070000Z\r\n", flat)
        self.assertIn("\r\nDTEND:20260301T073000Z\r\n", flat)
        self.assertIn("\r\nDTSTAMP:20260228T120000Z\r\n", flat)
        self.assertIn("\r\nSUMMARY:Handover\; ward 3\\, night team", flat)


if __name__ == "__main__":
    unittest.main()
