import unittest

from transit.alternatives import alternative_routes
from transit.model import Network


def triangle():
    """A, B, C form a triangle; D hangs off C."""
    net = Network()
    for stop_id in "ABCD":
        net.add_stop(stop_id, "Stop " + stop_id)
    net.add_link("A", "B", "T1", 1)
    net.add_link("B", "C", "T1", 1)
    net.add_link("C", "A", "T2", 1)
    net.add_link("C", "D", "T1", 1)
    return net


class AlternativesTest(unittest.TestCase):

    def test_best_route_first(self):
        routes = alternative_routes(triangle(), "A", "D", k=1)
        self.assertEqual([r.stops for r in routes], [["A", "C", "D"]])

    def test_no_loops(self):
        routes = alternative_routes(triangle(), "A", "D", k=3)
        self.assertEqual([r.stops for r in routes], [["A", "C", "D"], ["A", "B", "C", "D"]])


if __name__ == "__main__":
    unittest.main()
