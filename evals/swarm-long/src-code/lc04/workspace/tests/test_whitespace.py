"""Whitespace control with ``-`` markers."""

import unittest

from stencil import Environment


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class WhitespaceTests(unittest.TestCase):

    def test_block_markers_strip_newlines(self):
        src = "Items:\n{%- for x in xs %}\n  - {{ x }}\n{%- endfor %}\nEnd"
        self.assertEqual(render(src, xs=[1, 2]), "Items:\n  - 1\n  - 2\nEnd")

    def test_trailing_marker_strips_following_newline(self):
        self.assertEqual(render("{% if true -%}\n   yes\n{%- endif %}"), "yes")

    def test_no_marker_keeps_whitespace(self):
        self.assertEqual(render("a\n{% if true %} b {% endif %}\n"), "a\n b \n")


if __name__ == "__main__":
    unittest.main()
