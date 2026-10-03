import unittest

from throttle.metrics import Metrics
from throttle.report import render


def feed(metrics, tenant, allowed, denied):
    for _ in range(allowed):
        metrics.record(tenant, True)
    for _ in range(denied):
        metrics.record(tenant, False)


class ReportTest(unittest.TestCase):
    def test_most_denied_first(self):
        metrics = Metrics()
        feed(metrics, "acme", 3, 1)
        feed(metrics, "globex", 2, 2)
        lines = render(metrics).splitlines()
        self.assertEqual(lines[0], "tenant        allowed   denied   deny%")
        self.assertEqual(lines[1], "globex              2        2    50.0")
        self.assertEqual(lines[2], "acme                3        1    25.0")
        self.assertEqual(lines[3], "TOTAL               5        3    37.5")

    def test_equal_rows_alphabetical(self):
        metrics = Metrics()
        feed(metrics, "beta", 2, 2)
        feed(metrics, "alpha", 2, 2)
        feed(metrics, "gamma", 2, 2)
        names = [line.split()[0] for line in render(metrics).splitlines()[1:4]]
        self.assertEqual(names, ["alpha", "beta", "gamma"])


if __name__ == "__main__":
    unittest.main()
