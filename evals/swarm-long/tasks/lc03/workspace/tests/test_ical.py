import unittest
from datetime import datetime

from rota.ical import escape_text, export_calendar, format_dt
from rota.models import Shift


class IcalTest(unittest.TestCase):

    def test_escape_comma_and_semicolon(self):
        self.assertEqual(escape_text("Ward A, B; C"), "Ward A\\, B\; C")

    def test_escape_newline(self):
        self.assertEqual(escape_text("line one\nline two"), "line one\\nline two")

    def test_naive_datetime_is_floating(self):
        self.assertEqual(format_dt(datetime(2026, 3, 1, 9, 5)), "20260301T090500")

    def test_export_shape(self):
        s = Shift("S1", datetime(2026, 3, 2, 6), datetime(2026, 3, 2, 14), "nurse",
                  location="Ward 3", employee="E1")
        text = export_calendar([s], dtstamp=datetime(2026, 3, 1, 0, 0))
        self.assertTrue(text.startswith("BEGIN:VCALENDAR\r\n"))
        self.assertTrue(text.endswith("END:VCALENDAR\r\n"))
        self.assertIn("UID:S1@rota.local\r\n", text)
        self.assertIn("SUMMARY:nurse shift (E1)\r\n", text)


if __name__ == "__main__":
    unittest.main()
