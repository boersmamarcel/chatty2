import unittest
from decimal import Decimal

from transit.errors import FareError
from transit.fares import FareTable, fare_for_route
from transit.model import Network, route_from_path


def zoned_network():
    net = Network()
    net.add_stop("A", "Airport", "1")
    net.add_stop("B", "Bridge", "2")
    net.add_stop("C", "Castle", "1")
    net.add_stop("D", "Dock", "1")
    net.add_link("A", "B", "R", 5)
    net.add_link("B", "C", "R", 5)
    net.add_link("C", "D", "R", 5)
    return net


TABLE = FareTable({1: "2.40", 2: "3.10", 3: "3.80"}, "0.85")


class FaresTest(unittest.TestCase):

    def test_single_zone(self):
        net = zoned_network()
        route = route_from_path(net, ["C", "D"], ["R"])
        self.assertEqual(fare_for_route(net, route, TABLE), Decimal("2.40"))

    def test_unknown_concession(self):
        net = zoned_network()
        route = route_from_path(net, ["C", "D"], ["R"])
        with self.assertRaises(FareError):
            fare_for_route(net, route, TABLE, "student")

    def test_reentering_a_zone_counts_once(self):
        net = zoned_network()
        route = route_from_path(net, ["A", "B", "C"], ["R", "R"])
        self.assertEqual(fare_for_route(net, route, TABLE), Decimal("3.10"))


if __name__ == "__main__":
    unittest.main()
