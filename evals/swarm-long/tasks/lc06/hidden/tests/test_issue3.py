import unittest

from transit.errors import TimetableError
from transit.timetable import earliest_arrival, load_timetable
from transit.timeutil import format_time, parse_time

H = 3600
M = 60

NIGHT = """\
trip,line,stop,arrive,depart
N1,N,A,,23:50
N1,N,B,24:10,24:11
N1,N,C,24:30,
N2,M,C,,24:30
N2,M,D,25:05:30,
"""

TIGHT = """\
trip,line,stop,arrive,depart
T1,L1,A,,08:00
T1,L1,B,08:10,08:11
T1,L1,E,08:40,
T2,L2,B,,08:12
T2,L2,C,08:20,
T3,L3,B,,08:13
T3,L3,C,08:30,
T4,L4,B,,08:10
T4,L4,D,08:25,
T5,L5,B,,08:11
T5,L5,D,08:35,
"""


class ParseFormatTest(unittest.TestCase):

    def test_parse_past_midnight(self):
        self.assertEqual(parse_time("24:05"), 86700)
        self.assertEqual(parse_time("25:30"), 91800)
        self.assertEqual(parse_time("47:59:59"), 172799)
        self.assertEqual(parse_time(" 00:00 "), 0)
        self.assertEqual(parse_time("23:59"), 86340)

    def test_parse_rejects(self):
        for bad in ("48:00", "48:00:00", "99:00", "12:60", "12:00:60", "1200", "", "ab:cd"):
            with self.assertRaises(ValueError, msg=bad):
                parse_time(bad)

    def test_format_no_wrap(self):
        self.assertEqual(format_time(91800), "25:30")
        self.assertEqual(format_time(86400 + 61), "24:01:01")
        self.assertEqual(format_time(86400), "24:00")
        self.assertEqual(format_time(172799), "47:59:59")
        self.assertEqual(format_time(8 * H + 5 * M), "08:05")
        with self.assertRaises(ValueError):
            format_time(-1)

    def test_round_trip(self):
        for text in ("00:00", "07:05", "23:59:59", "24:00", "30:15:01", "47:00"):
            self.assertEqual(format_time(parse_time(text)), text)


class EarliestArrivalTest(unittest.TestCase):

    def test_night_timetable_loads(self):
        tt = load_timetable(NIGHT)
        self.assertEqual(tt.trips(), ["N1", "N2"])
        self.assertEqual(tt.connections[-1].arr, 25 * H + 5 * M + 30)

    def test_still_rejects_hour_48(self):
        with self.assertRaises(TimetableError):
            load_timetable("trip,line,stop,arrive,depart\nX,L,A,,47:00\nX,L,B,48:00,\n")

    def test_night_journey(self):
        tt = load_timetable(NIGHT)
        journey = earliest_arrival(tt, "A", "C", 23 * H + 40 * M)
        self.assertEqual(journey.arrival, 24 * H + 30 * M)
        self.assertEqual(journey.trips, ["N1"])
        self.assertEqual(format_time(journey.arrival), "24:30")

    def test_night_transfer_inclusive(self):
        tt = load_timetable(NIGHT)
        # N1 reaches C at 24:30 and N2 leaves C at 24:30.
        journey = earliest_arrival(tt, "A", "D", 23 * H + 50 * M)
        self.assertIsNotNone(journey)
        self.assertEqual(journey.trips, ["N1", "N2"])
        self.assertEqual(journey.arrival, 25 * H + 5 * M + 30)
        self.assertIsNone(earliest_arrival(tt, "A", "D", 23 * H + 50 * M, min_transfer=1))

    def test_origin_boundary_inclusive(self):
        tt = load_timetable(TIGHT)
        journey = earliest_arrival(tt, "A", "B", 8 * H)
        self.assertEqual(journey.arrival, 8 * H + 10 * M)
        self.assertIsNone(earliest_arrival(tt, "A", "B", 8 * H + 1))

    def test_transfer_exactly_min_transfer(self):
        tt = load_timetable(TIGHT)
        journey = earliest_arrival(tt, "A", "C", 8 * H, min_transfer=120)
        self.assertEqual(journey.trips, ["T1", "T2"])
        self.assertEqual(journey.arrival, 8 * H + 20 * M)

    def test_transfer_one_second_short(self):
        tt = load_timetable(TIGHT)
        journey = earliest_arrival(tt, "A", "C", 8 * H, min_transfer=121)
        self.assertEqual(journey.trips, ["T1", "T3"])
        self.assertEqual(journey.arrival, 8 * H + 30 * M)

    def test_zero_transfer_same_second(self):
        tt = load_timetable(TIGHT)
        journey = earliest_arrival(tt, "A", "D", 8 * H)
        self.assertEqual(journey.trips, ["T1", "T4"])
        self.assertEqual(journey.arrival, 8 * H + 25 * M)
        journey = earliest_arrival(tt, "A", "D", 8 * H, min_transfer=60)
        self.assertEqual(journey.trips, ["T1", "T5"])
        self.assertEqual(journey.arrival, 8 * H + 35 * M)

    def test_staying_on_trip_needs_no_time(self):
        tt = load_timetable(TIGHT)
        journey = earliest_arrival(tt, "A", "E", 8 * H, min_transfer=3600)
        self.assertEqual(journey.trips, ["T1"])
        self.assertEqual(journey.arrival, 8 * H + 40 * M)
        self.assertEqual([c.to_stop for c in journey.connections], ["B", "E"])

    def test_unreachable(self):
        tt = load_timetable(TIGHT)
        self.assertIsNone(earliest_arrival(tt, "C", "A", 8 * H))
        self.assertIsNone(earliest_arrival(tt, "A", "C", 9 * H))


if __name__ == "__main__":
    unittest.main()
