import unittest

from csvline import split_line


class SplitLineTest(unittest.TestCase):
    def test_plain(self):
        self.assertEqual(split_line("a,b,c"), ["a", "b", "c"])

    def test_quoted_comma(self):
        self.assertEqual(split_line('x,"1,5",y'), ["x", "1,5", "y"])

    def test_doubled_quote(self):
        self.assertEqual(split_line('"say ""hi""",2'), ['say "hi"', "2"])

    def test_empty_fields(self):
        self.assertEqual(split_line(",,"), ["", "", ""])
