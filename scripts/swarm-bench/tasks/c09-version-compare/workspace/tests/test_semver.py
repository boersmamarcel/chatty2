import unittest

from semver import compare


class CompareTest(unittest.TestCase):
    def test_equal(self):
        self.assertEqual(compare("1.2.3", "1.2.3"), 0)

    def test_numeric_not_lexical(self):
        self.assertEqual(compare("1.10.0", "1.9.0"), 1)
        self.assertEqual(compare("2.0.0", "10.0.0"), -1)

    def test_prerelease_before_release(self):
        self.assertEqual(compare("1.0.0-rc1", "1.0.0"), -1)
        self.assertEqual(compare("1.0.0", "1.0.0-rc1"), 1)

    def test_prereleases(self):
        self.assertEqual(compare("1.0.0-alpha", "1.0.0-beta"), -1)
