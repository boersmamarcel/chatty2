import unittest

from verspec.bump import bump


class BumpTest(unittest.TestCase):
    def test_release(self):
        self.assertEqual(str(bump("1.2.3", "major")), "2.0.0")
        self.assertEqual(str(bump("1.2.3", "minor")), "1.3.0")
        self.assertEqual(str(bump("1.2.3", "patch")), "1.2.4")

    def test_prerelease_to_release(self):
        self.assertEqual(str(bump("1.2.3-rc.1", "patch")), "1.2.3")

    def test_prerelease_part(self):
        self.assertEqual(str(bump("1.2.3-rc.1", "prerelease")), "1.2.3-rc.2")
        self.assertEqual(str(bump("1.2.3", "prerelease")), "1.2.4-rc.0")


if __name__ == "__main__":
    unittest.main()
