import unittest
from decimal import Decimal

from transit.errors import FareError
from transit.fares import FareTable, count_zones, fare_for_route
from transit.model import Network, route_from_path

TABLE = FareTable({1: "2.40", 2: "3.10", 3: "3.80"}, "0.85")


def zoned():
    net = Network()
    for stop_id, zone in (("A", "1"), ("B", "2"), ("C", "1"), ("D", "3"), ("E", "4"),
                          ("F", "2"), ("N", None), ("M", None), ("G", "1")):
        net.add_stop(stop_id, "Stop " + stop_id, zone)
    for a, b in (("A", "B"), ("B", "C"), ("C", "D"), ("D", "E"), ("E", "F"),
                 ("A", "N"), ("N", "C"), ("C", "M"), ("F", "A"), ("A", "G")):
        net.add_link(a, b, "R", 5)
    return net


def route(net, stops):
    return route_from_path(net, list(stops), ["R"] * (len(stops) - 1))


class Issue4Test(unittest.TestCase):

    def test_count_zones(self):
        self.assertEqual(count_zones(["1", "2", "1"]), 2)
        self.assertEqual(count_zones(["1"]), 1)
        self.assertEqual(count_zones(["1", "1", "1"]), 1)
        self.assertEqual(count_zones(["1", "2", "1", "2", "3"]), 3)
        self.assertEqual(count_zones(["A", "B", "C", "A"]), 3)

    def test_distinct_zones(self):
        net = zoned()
        self.assertEqual(fare_for_route(net, route(net, "ABC"), TABLE), Decimal("3.10"))
        self.assertEqual(fare_for_route(net, route(net, "ABCD"), TABLE), Decimal("3.80"))
        # zones 1,2,1,3,4,2,1 -> 4 distinct zones -> 3.80 + 0.85
        self.assertEqual(fare_for_route(net, route(net, "ABCDEFA"), TABLE), Decimal("4.65"))

    def test_child_rounds_half_up(self):
        net = zoned()
        self.assertEqual(fare_for_route(net, route(net, "CDEF"), TABLE, "child"), Decimal("2.33"))
        self.assertEqual(fare_for_route(net, route(net, "ABC"), TABLE, "child"), Decimal("1.55"))

    def test_senior_rounds_half_up(self):
        table = FareTable({1: "1.70", 2: "2.30"}, "1.00")
        net = zoned()
        # 1.70 * 0.65 = 1.105 -> 1.11
        self.assertEqual(fare_for_route(net, route(net, "AG"), table, "senior"),
                         Decimal("1.11"))
        # 2.30 * 0.65 = 1.495 -> 1.50
        self.assertEqual(fare_for_route(net, route(net, "ABC"), table, "senior"), Decimal("1.50"))

    def test_custom_concession_half_up(self):
        table = FareTable({1: "2.50"}, "0.25", concessions={"adult": 0, "promo": 15})
        net = zoned()
        # 2.50 * 0.85 = 2.125 -> 2.13
        self.assertEqual(fare_for_route(net, route(net, "GA"), table, "promo"),
                         Decimal("2.13"))

    def test_result_is_decimal_with_cents(self):
        net = zoned()
        price = fare_for_route(net, route(net, "ABC"), TABLE, "adult")
        self.assertIsInstance(price, Decimal)
        self.assertEqual(str(price), "3.10")

    def test_missing_zone(self):
        net = zoned()
        with self.assertRaises(FareError) as ctx:
            fare_for_route(net, route(net, "ANC"), TABLE)
        self.assertEqual(str(ctx.exception), "stop 'N' has no zone")

    def test_missing_zone_first_in_travel_order(self):
        net = zoned()
        with self.assertRaises(FareError) as ctx:
            fare_for_route(net, route(net, "MCNA"), TABLE)
        self.assertEqual(str(ctx.exception), "stop 'M' has no zone")

    def test_unknown_concession(self):
        net = zoned()
        with self.assertRaises(FareError) as ctx:
            fare_for_route(net, route(net, "ABC"), TABLE, "student")
        self.assertEqual(str(ctx.exception), "unknown concession 'student'")


if __name__ == "__main__":
    unittest.main()
