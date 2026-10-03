import unittest

from transit.alternatives import alternative_routes
from transit.errors import UnknownStopError
from transit.model import Network


def make(stops, links):
    net = Network()
    for stop_id in stops:
        net.add_stop(stop_id, "Stop " + stop_id)
    for link in links:
        net.add_link(*link)
    return net


def triangle():
    return make("ABCD", [("A", "B", "T1", 1), ("B", "C", "T1", 1),
                         ("C", "A", "T2", 1), ("C", "D", "T1", 1)])


def grid():
    """Two squares sharing an edge plus a parallel line; plenty of cycles."""
    return make("ABCDEF", [
        ("A", "B", "R", 2), ("B", "C", "R", 2),
        ("A", "D", "G", 3), ("D", "E", "G", 1), ("E", "F", "G", 3),
        ("B", "E", "Y", 1), ("C", "F", "Y", 2),
        ("A", "B", "X", 2),
    ])


class Issue6Test(unittest.TestCase):

    def assert_loopless(self, routes):
        for route in routes:
            self.assertEqual(len(route.stops), len(set(route.stops)), route.stops)

    def test_triangle(self):
        routes = alternative_routes(triangle(), "A", "D", k=5)
        self.assertEqual([r.stops for r in routes], [["A", "C", "D"], ["A", "B", "C", "D"]])
        self.assertEqual([r.cost for r in routes], [2, 3])

    def test_origin_never_revisited(self):
        routes = alternative_routes(triangle(), "B", "D", k=10)
        self.assert_loopless(routes)
        self.assertEqual([r.stops for r in routes], [["B", "C", "D"], ["B", "A", "C", "D"]])

    def test_grid_all_loopless_and_sorted(self):
        routes = alternative_routes(grid(), "A", "F", k=50, transfer_penalty=1)
        self.assert_loopless(routes)
        keys = [r.key() for r in routes]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(len(set(keys)), len(keys))
        self.assertEqual([(r.stops, r.lines, r.cost) for r in routes], [
            (["A", "D", "E", "F"], ["G", "G", "G"], 7),
            (["A", "B", "C", "F"], ["R", "R", "Y"], 7),
            (["A", "B", "C", "F"], ["X", "R", "Y"], 8),
            (["A", "B", "E", "F"], ["R", "Y", "G"], 8),
            (["A", "B", "E", "F"], ["X", "Y", "G"], 8),
            (["A", "D", "E", "B", "C", "F"], ["G", "G", "Y", "R", "Y"], 12),
        ])

    def test_grid_top_three(self):
        routes = alternative_routes(grid(), "A", "F", k=3)
        self.assertEqual([(r.stops, r.lines, r.cost) for r in routes], [
            (["A", "B", "C", "F"], ["R", "R", "Y"], 6),
            (["A", "B", "C", "F"], ["X", "R", "Y"], 6),
            (["A", "B", "E", "F"], ["R", "Y", "G"], 6),
        ])

    def test_first_boarding_free(self):
        net = make("ABC", [("A", "B", "L", 4), ("B", "C", "L", 6)])
        routes = alternative_routes(net, "A", "C", k=2, transfer_penalty=7)
        self.assertEqual([r.cost for r in routes], [10])

    def test_max_extra_inclusive(self):
        routes = alternative_routes(triangle(), "A", "D", k=5, max_extra=1)
        self.assertEqual([r.cost for r in routes], [2, 3])
        routes = alternative_routes(triangle(), "A", "D", k=5, max_extra=0)
        self.assertEqual([r.cost for r in routes], [2])

    def test_max_extra_with_k(self):
        routes = alternative_routes(grid(), "A", "F", k=2, max_extra=10)
        self.assertEqual(len(routes), 2)
        routes = alternative_routes(grid(), "A", "F", k=50, max_extra=1)
        self.assertTrue(all(r.cost <= 7 for r in routes))
        self.assertIn(7, [r.cost for r in routes])
        self.assert_loopless(routes)

    def test_k_validation(self):
        for k in (0, -1):
            with self.assertRaises(ValueError) as ctx:
                alternative_routes(triangle(), "A", "D", k=k)
            self.assertEqual(str(ctx.exception), "k must be at least 1")

    def test_k_checked_before_stops(self):
        with self.assertRaises(ValueError):
            alternative_routes(triangle(), "A", "NOPE", k=0)

    def test_unknown_stop(self):
        with self.assertRaises(UnknownStopError):
            alternative_routes(triangle(), "A", "NOPE", k=2)

    def test_unreachable(self):
        net = make("ABCZ", [("A", "B", "L", 1), ("B", "C", "L", 1)])
        self.assertEqual(alternative_routes(net, "A", "Z", k=3), [])


if __name__ == "__main__":
    unittest.main()
