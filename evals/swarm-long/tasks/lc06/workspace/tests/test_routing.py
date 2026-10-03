import unittest

from transit.dijkstra import shortest_route
from transit.errors import NoRouteError
from transit.model import Network


def line_network():
    net = Network()
    for stop_id in "ABCD":
        net.add_stop(stop_id, "Stop " + stop_id)
    net.add_link("A", "B", "L1", 4)
    net.add_link("B", "C", "L1", 6)
    return net


class RoutingTest(unittest.TestCase):

    def test_simple_route(self):
        route = shortest_route(line_network(), "A", "C")
        self.assertEqual(route.stops, ["A", "B", "C"])
        self.assertEqual(route.cost, 10)

    def test_unreachable(self):
        with self.assertRaises(NoRouteError):
            shortest_route(line_network(), "A", "D")

    def test_first_boarding_is_not_a_transfer(self):
        route = shortest_route(line_network(), "A", "C", transfer_penalty=3)
        self.assertEqual(route.stops, ["A", "B", "C"])
        self.assertEqual(route.cost, 10)


if __name__ == "__main__":
    unittest.main()
