"""Issue 4: error positions and message format."""

import unittest

from stencil import DictLoader, Environment, TemplateSyntaxError
from stencil.utils import position_of


class PositionTests(unittest.TestCase):

    def test_first_character(self):
        self.assertEqual(position_of("abc", 0), (1, 1))
        self.assertEqual(position_of("abc", 2), (1, 3))

    def test_newline(self):
        self.assertEqual(position_of("ab\ncd", 3), (2, 1))
        self.assertEqual(position_of("ab\ncd", 4), (2, 2))
        self.assertEqual(position_of("a\n\n\nb", 4), (4, 1))

    def test_crlf_is_one_break(self):
        self.assertEqual(position_of("a\r\nb", 3), (2, 1))
        self.assertEqual(position_of("a\r\n\r\nb", 5), (3, 1))
        self.assertEqual(position_of("a\r\nbcd", 5), (2, 3))

    def test_lone_cr(self):
        self.assertEqual(position_of("a\rb", 2), (2, 1))
        self.assertEqual(position_of("a\rb\nc\r\nd", 7), (4, 1))

    def test_tab_is_one_column(self):
        self.assertEqual(position_of("\t\tx", 2), (1, 3))


class ErrorFormatTests(unittest.TestCase):

    def test_attributes_and_str(self):
        with self.assertRaises(TemplateSyntaxError) as cm:
            Environment().from_string("a\n  {% frob %}")
        err = cm.exception
        self.assertEqual((err.lineno, err.col), (2, 3))
        self.assertEqual(err.message, "unknown tag 'frob'")
        self.assertIsNone(err.name)
        self.assertEqual(str(err), "<string>:2:3: unknown tag 'frob'")

    def test_direct_construction(self):
        self.assertEqual(str(TemplateSyntaxError("boom", 3, 7, "a.txt")), "a.txt:3:7: boom")
        self.assertEqual(str(TemplateSyntaxError("boom", 3, 7)), "<string>:3:7: boom")

    def test_named_template_with_crlf(self):
        env = Environment(loader=DictLoader({"page.html": "x\r\n{{ y"}))
        with self.assertRaises(TemplateSyntaxError) as cm:
            env.get_template("page.html")
        self.assertEqual(cm.exception.name, "page.html")
        self.assertEqual(str(cm.exception), "page.html:2:1: unclosed output tag")

    def test_unclosed_block_points_at_opening_tag(self):
        with self.assertRaises(TemplateSyntaxError) as cm:
            Environment().from_string("x\r  {% for a in b %}y\r\nz")
        self.assertEqual(str(cm.exception), "<string>:2:3: unclosed 'for' block")

    def test_unexpected_end_tag(self):
        with self.assertRaises(TemplateSyntaxError) as cm:
            Environment().from_string("\t{% endif %}")
        self.assertEqual(str(cm.exception), "<string>:1:2: unexpected 'endif'")

    def test_expression_error_points_at_tag(self):
        with self.assertRaises(TemplateSyntaxError) as cm:
            Environment().from_string("ok\r\nok\r\n   {{ 1 + }}")
        err = cm.exception
        self.assertEqual((err.lineno, err.col), (3, 4))
        self.assertEqual(str(err), "<string>:3:4: unexpected end of expression")


if __name__ == "__main__":
    unittest.main()
