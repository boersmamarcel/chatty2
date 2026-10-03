import unittest

from verspec.resolve import sort_versions
from verspec.version import Version, VersionError

CHAIN = ["1.0.0-1", "1.0.0-2", "1.0.0-10", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta",
         "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1-rc.0",
         "1.0.1"]


class OrderTest(unittest.TestCase):
    def test_chain(self):
        versions = [Version.parse(v) for v in CHAIN]
        for low, high in zip(versions, versions[1:]):
            self.assertLess(low, high, msg="%s < %s" % (low, high))
            self.assertGreater(high, low)
            self.assertNotEqual(low, high)

    def test_sort(self):
        shuffled = [CHAIN[i] for i in (7, 0, 12, 3, 9, 1, 11, 5, 2, 10, 4, 8, 6)]
        self.assertEqual([str(v) for v in sort_versions(shuffled)], CHAIN)

    def test_numeric_vs_alpha_identifier(self):
        self.assertLess(Version.parse("1.0.0-rc.9"), Version.parse("1.0.0-rc.10"))
        self.assertLess(Version.parse("1.0.0-rc.99"), Version.parse("1.0.0-rc.a"))
        self.assertLess(Version.parse("1.0.0-9.z"), Version.parse("1.0.0-10.a"))
        self.assertLess(Version.parse("1.0.0-x-1"), Version.parse("1.0.0-x-2"))

    def test_build_ignored(self):
        a = Version.parse("1.0.0-rc.1+b.7")
        b = Version.parse("1.0.0-rc.1")
        self.assertEqual(a, b)
        self.assertEqual(hash(a), hash(b))
        self.assertEqual(len({a, b, Version.parse("1.0.0-rc.1+other")}), 1)
        self.assertEqual(Version.parse("2.0.0+x"), Version.parse("2.0.0"))

    def test_invalid_identifiers(self):
        for bad in ["1.0.0-rc..1", "1.0.0-rc.", "1.0.0-.rc", "1.0.0+b..1", "1.0.0+", "1.0.0-rc.01",
                    "1.0.0-01", "1.0.0-rc.1+b."]:
            with self.assertRaises(VersionError, msg=bad):
                Version.parse(bad)

    def test_still_valid(self):
        for good in ["1.0.0-0", "1.0.0-rc.0", "1.0.0-0a", "1.0.0+001", "1.0.0-alpha-1.2",
                     "1.0.0-rc.1+build.007"]:
            self.assertEqual(str(Version.parse(good)), good)


if __name__ == "__main__":
    unittest.main()
