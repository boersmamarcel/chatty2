import unittest

from transit.dijkstra import shortest_route
from transit.errors import NoRouteError, UnknownStopError
from transit.model import Network


def make(stops, links):
    net = Network()
    for stop_id in stops:
        net.add_stop(stop_id, "Stop " + stop_id)
    for a, b, line, minutes in links:
        net.add_link(a, b, line, minutes)
    return net


class Issue2Test(unittest.TestCase):

    def test_first_boarding_free(self):
        net = make("ABC", [("A", "B", "L1", 4), ("B", "C", "L1", 6)])
        for penalty in (0, 3, 50):
            route = shortest_route(net, "A", "C", transfer_penalty=penalty)
            self.assertEqual(route.cost, 10)
            self.assertEqual(route.stops, ["A", "B", "C"])

    def test_single_hop_cost(self):
        net = make("AB", [("A", "B", "L1", 7)])
        self.assertEqual(shortest_route(net, "A", "B", transfer_penalty=5).cost, 7)

    def test_penalty_per_change(self):
        net = make("ABCD", [("A", "B", "L1", 4), ("B", "C", "L2", 6), ("C", "D", "L3", 2)])
        route = shortest_route(net, "A", "D", transfer_penalty=5)
        self.assertEqual(route.cost, 4 + 6 + 2 + 2 * 5)
        self.assertEqual([l.line for l in route.links], ["L1", "L2", "L3"])

    def test_penalty_changes_choice(self):
        # Direct line: 12 minutes. Faster with a change: 10 minutes + penalty.
        net = make("ABCD", [("A", "B", "S", 6), ("B", "D", "S", 6),
                            ("A", "C", "F1", 5), ("C", "D", "F2", 5)])
        route = shortest_route(net, "A", "D", transfer_penalty=4)
        self.assertEqual(route.stops, ["A", "B", "D"])
        self.assertEqual(route.cost, 12)
        route = shortest_route(net, "A", "D", transfer_penalty=1)
        self.assertEqual(route.stops, ["A", "C", "D"])
        self.assertEqual(route.cost, 11)

    def test_tie_fewer_changes_wins(self):
        net = make("ABDX", [("A", "B", "T1", 5), ("B", "D", "T2", 5),
                            ("A", "X", "T9", 5), ("X", "D", "T9", 5)])
        route = shortest_route(net, "A", "D")
        self.assertEqual(route.stops, ["A", "X", "D"])
        self.assertEqual(route.cost, 10)
        self.assertEqual([l.line for l in route.links], ["T9", "T9"])

    def test_tie_with_penalty_fewer_changes(self):
        # Both cost 12 with penalty 2: A-B-D has one change (10 + 2),
        # A-Y-D none (12).
        net = make("ABDY", [("A", "B", "T1", 5), ("B", "D", "T2", 5),
                            ("A", "Y", "T9", 6), ("Y", "D", "T9", 6)])
        route = shortest_route(net, "A", "D", transfer_penalty=2)
        self.assertEqual(route.stops, ["A", "Y", "D"])
        self.assertEqual(route.cost, 12)

    def test_tie_smallest_stop_sequence(self):
        net = make("ABCD", [("A", "C", "T1", 5), ("C", "D", "T1", 5),
                            ("A", "B", "T2", 5), ("B", "D", "T2", 5)])
        route = shortest_route(net, "A", "D")
        self.assertEqual(route.stops, ["A", "B", "D"])
        self.assertEqual([l.line for l in route.links], ["T2", "T2"])

    def test_tie_smallest_stop_sequence_longer_path(self):
        # A-B-C-E and A-D-E both cost 6 with no change; ['A','B',...] < ['A','D',...]
        net = make("ABCDE", [("A", "D", "Z", 3), ("D", "E", "Z", 3),
                             ("A", "B", "Y", 2), ("B", "C", "Y", 2), ("C", "E", "Y", 2)])
        route = shortest_route(net, "A", "E")
        self.assertEqual(route.stops, ["A", "B", "C", "E"])

    def test_tie_smallest_line_sequence(self):
        net = make("ABD", [("A", "B", "T2", 5), ("A", "B", "T1", 5),
                           ("B", "D", "T2", 5), ("B", "D", "T1", 5)])
        route = shortest_route(net, "A", "D", transfer_penalty=1)
        self.assertEqual([l.line for l in route.links], ["T1", "T1"])
        self.assertEqual(route.cost, 10)

    def test_unknown_destination(self):
        net = make("AB", [("A", "B", "L1", 1)])
        with self.assertRaises(UnknownStopError) as ctx:
            shortest_route(net, "A", "Q")
        self.assertEqual(ctx.exception.stop_id, "Q")

    def test_unknown_origin(self):
        net = make("AB", [("A", "B", "L1", 1)])
        with self.assertRaises(UnknownStopError) as ctx:
            shortest_route(net, "Q", "A")
        self.assertEqual(ctx.exception.stop_id, "Q")

    def test_unreachable_and_same_stop(self):
        net = make("ABC", [("A", "B", "L1", 1)])
        with self.assertRaises(NoRouteError):
            shortest_route(net, "A", "C")
        route = shortest_route(net, "B", "B", transfer_penalty=4)
        self.assertEqual((route.stops, route.links, route.cost), (["B"], [], 0))


if __name__ == "__main__":
    unittest.main()
