"""Issue 2: whitespace control."""

import unittest

from stencil import Environment


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class BlockMarkerTests(unittest.TestCase):

    def test_leading_marker_strips_all_whitespace(self):
        self.assertEqual(render("x \t\n\r\n  {%- if true %}y{% endif %}"), "xy")

    def test_trailing_marker_strips_all_whitespace(self):
        self.assertEqual(render("{% if true -%} \n\t\n z{% endif %}"), "z")

    def test_whitespace_between_two_markers_disappears(self):
        self.assertEqual(render("[{% if true -%}  \n  {%- endif %}]"), "[]")

    def test_marker_affects_only_its_side(self):
        self.assertEqual(render("a \n{%- if true %} \nb{% endif %}"), "a \nb")
        self.assertEqual(render("a \n{% if true -%} \nb{% endif %}"), "a \nb")

    def test_list_example(self):
        src = "<ul>\n  {%- for i in [1, 2] %}\n  <li>{{ i }}</li>\n  {%- endfor %}\n</ul>"
        self.assertEqual(render(src), "<ul>\n  <li>1</li>\n  <li>2</li>\n</ul>")

    def test_end_of_template(self):
        self.assertEqual(render("a{% if true %}b{% endif -%}  \n\n"), "ab")


class OutputAndCommentMarkerTests(unittest.TestCase):

    def test_output_markers(self):
        self.assertEqual(render("a \n {{- x -}} \n b", x="X"), "aXb")

    def test_output_marker_end_of_template(self):
        self.assertEqual(render("a {{ 'b' -}}  \n\n"), "a b")

    def test_comment_markers(self):
        self.assertEqual(render("a\n  {#- note -#}\n  b"), "ab")
        self.assertEqual(render("a\n{# note -#}\nb"), "a\nb")

    def test_mixed(self):
        src = "Dear {{ name -}}\n,\n{#- greeting -#}\n\nThanks"
        self.assertEqual(render(src, name="Ann"), "Dear Ann,Thanks")


class MinusIsNotAlwaysAMarker(unittest.TestCase):

    def test_negative_number(self):
        self.assertEqual(render("[{{ -1 }}]"), "[-1]")
        self.assertEqual(render("a {{ -n }} b", n=4), "a -4 b")

    def test_subtraction(self):
        self.assertEqual(render("x {{ 5 - 3 }} y"), "x 2 y")

    def test_set_negative(self):
        self.assertEqual(render("a {% set n = -2 %} b{{ n }}"), "a  b-2")

    def test_marker_and_negative(self):
        self.assertEqual(render("a  {{- -1 -}}  b"), "a-1b")


if __name__ == "__main__":
    unittest.main()
