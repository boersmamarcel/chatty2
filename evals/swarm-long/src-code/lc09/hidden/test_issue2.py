import unittest

from verspec.constraints import parse_constraint
from verspec.version import Version


def ok(constraint, version):
    return parse_constraint(constraint).matches(Version.parse(version))


class CaretTest(unittest.TestCase):
    def test_major_one(self):
        self.assertTrue(ok("^1.2.3", "1.2.3"))
        self.assertTrue(ok("^1.2.3", "1.99.0"))
        self.assertFalse(ok("^1.2.3", "2.0.0"))
        self.assertFalse(ok("^1.2.3", "1.2.2"))

    def test_major_zero(self):
        self.assertTrue(ok("^0.2.3", "0.2.3"))
        self.assertTrue(ok("^0.2.3", "0.2.99"))
        self.assertFalse(ok("^0.2.3", "0.3.0"))
        self.assertFalse(ok("^0.2.3", "0.9.0"))
        self.assertFalse(ok("^0.2.3", "1.0.0"))
        self.assertFalse(ok("^0.2.3", "0.2.2"))

    def test_minor_zero(self):
        self.assertTrue(ok("^0.0.3", "0.0.3"))
        self.assertFalse(ok("^0.0.3", "0.0.4"))
        self.assertFalse(ok("^0.0.3", "0.1.0"))
        self.assertTrue(ok("^0.0.0", "0.0.0"))
        self.assertFalse(ok("^0.0.0", "0.0.1"))

    def test_prerelease_lower_bound(self):
        self.assertTrue(ok("^0.2.3-rc.1", "0.2.3-rc.1"))
        self.assertTrue(ok("^0.2.3-rc.1", "0.2.9"))
        self.assertFalse(ok("^0.2.3-rc.1", "0.3.0"))
        self.assertTrue(ok("^2.0.0-rc.1", "2.5.0"))
        self.assertFalse(ok("^2.0.0-rc.1", "3.0.0"))

    def test_combined(self):
        self.assertTrue(ok("^0.2.3, !=0.2.5", "0.2.4"))
        self.assertFalse(ok("^0.2.3, !=0.2.5", "0.2.5"))
        self.assertFalse(ok("^0.2.3, !=0.2.5", "0.4.0"))
        self.assertTrue(ok(">=0.1.0, ^0.1.5", "0.1.7"))
        self.assertFalse(ok(">=0.1.0, ^0.1.5", "0.2.0"))


if __name__ == "__main__":
    unittest.main()
