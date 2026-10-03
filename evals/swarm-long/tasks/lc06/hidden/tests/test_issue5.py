import unittest

from transit.formatting import format_duration, format_itinerary
from transit.itinerary import Leg, build_itinerary
from transit.model import Network, Route, route_from_path


def city():
    net = Network()
    for stop_id, name in (("CEN", "Central"), ("MUS", "Museum"), ("PRK", "Park"),
                          ("HAR", "Harbour"), ("ZOO", "Zoo")):
        net.add_stop(stop_id, name)
    net.add_link("CEN", "MUS", "T1", 30)
    net.add_link("MUS", "PRK", "T2", 35)
    net.add_link("PRK", "HAR", "T1", 5)
    net.add_link("HAR", "ZOO", "T1", 7)
    net.add_link("CEN", "ZOO", "BUS7", 70)
    return net


class Issue5Test(unittest.TestCase):

    def test_legs_follow_consecutive_runs(self):
        net = city()
        route = route_from_path(net, ["CEN", "MUS", "PRK", "HAR", "ZOO"], ["T1", "T2", "T1", "T1"])
        itin = build_itinerary(route)
        self.assertEqual(itin.legs, [
            Leg("T1", ["CEN", "MUS"], 30),
            Leg("T2", ["MUS", "PRK"], 35),
            Leg("T1", ["PRK", "HAR", "ZOO"], 12),
        ])
        self.assertEqual(itin.changes, 2)
        self.assertEqual(itin.lines, ["T1", "T2", "T1"])
        self.assertEqual(itin.minutes, 77)
        self.assertEqual(itin.transfer_stops(), ["MUS", "PRK"])

    def test_single_line_route(self):
        net = city()
        route = route_from_path(net, ["PRK", "HAR", "ZOO"], ["T1", "T1"])
        itin = build_itinerary(route)
        self.assertEqual(itin.legs, [Leg("T1", ["PRK", "HAR", "ZOO"], 12)])
        self.assertEqual(itin.changes, 0)

    def test_no_links(self):
        itin = build_itinerary(Route(["CEN"], [], 0))
        self.assertEqual(itin.legs, [])
        self.assertEqual(itin.changes, 0)

    def test_format_duration(self):
        cases = {0: "0 min", 5: "5 min", 59: "59 min", 60: "1 h 00 min", 65: "1 h 05 min",
                 70: "1 h 10 min", 119: "1 h 59 min", 125: "2 h 05 min", 600: "10 h 00 min"}
        for minutes, text in sorted(cases.items()):
            self.assertEqual(format_duration(minutes), text)
        with self.assertRaises(ValueError):
            format_duration(-1)

    def test_issue_example(self):
        net = city()
        route = route_from_path(net, ["CEN", "MUS", "PRK", "HAR"], ["T1", "T2", "T1"])
        self.assertEqual(format_itinerary(net, build_itinerary(route)), "\n".join([
            "Central -> Harbour: 1 h 10 min, 2 changes",
            "  T1   Central -> Museum (1 stop, 30 min)",
            "  T2   Museum -> Park (1 stop, 35 min)",
            "  T1   Park -> Harbour (1 stop, 5 min)",
        ]))

    def test_long_leg_and_line_name(self):
        net = city()
        route = route_from_path(net, ["CEN", "ZOO"], ["BUS7"])
        self.assertEqual(format_itinerary(net, build_itinerary(route)), "\n".join([
            "Central -> Zoo: 1 h 10 min, no changes",
            "  BUS7 Central -> Zoo (1 stop, 1 h 10 min)",
        ]))

    def test_mixed(self):
        net = city()
        route = route_from_path(net, ["ZOO", "HAR", "PRK", "MUS"], ["T1", "T1", "T2"])
        self.assertEqual(format_itinerary(net, build_itinerary(route)), "\n".join([
            "Zoo -> Museum: 47 min, 1 change",
            "  T1   Zoo -> Park (2 stops, 12 min)",
            "  T2   Park -> Museum (1 stop, 35 min)",
        ]))


if __name__ == "__main__":
    unittest.main()
