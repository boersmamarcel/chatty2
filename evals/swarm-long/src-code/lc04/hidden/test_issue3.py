"""Issue 3: escaping, Markup and the safe/escape filters."""

import unittest

from stencil import Environment, Markup, escape


def render(source, autoescape=False, **variables):
    return Environment(autoescape=autoescape).from_string(source).render(**variables)


class EscapeFunctionTests(unittest.TestCase):

    def test_all_five_characters(self):
        result = escape("<a href=\"x\">Tom & Jerry's</a>")
        self.assertIsInstance(result, Markup)
        self.assertEqual(result, "&lt;a href=&#34;x&#34;&gt;Tom &amp; Jerry&#39;s&lt;/a&gt;")

    def test_each_character_escaped_once(self):
        self.assertEqual(escape("<"), "&lt;")
        self.assertEqual(escape("&lt;"), "&amp;lt;")
        self.assertEqual(escape("&&"), "&amp;&amp;")
        self.assertEqual(escape("'"), "&#39;")

    def test_non_strings(self):
        self.assertEqual(escape(42), "42")
        self.assertIsInstance(escape(42), Markup)

    def test_markup_unchanged(self):
        self.assertEqual(escape(Markup("<b>&amp;</b>")), "<b>&amp;</b>")

        class Html(object):
            def __html__(self):
                return "<i>ok</i>"
        self.assertEqual(escape(Html()), "<i>ok</i>")


class MarkupTests(unittest.TestCase):

    def test_markup_plus_str(self):
        result = Markup("<b>") + "<"
        self.assertIsInstance(result, Markup)
        self.assertEqual(result, "<b>&lt;")

    def test_str_plus_markup(self):
        result = "it's " + Markup("<b>")
        self.assertIsInstance(result, Markup)
        self.assertEqual(result, "it&#39;s <b>")

    def test_markup_plus_markup(self):
        self.assertEqual(Markup("<b>") + Markup("</b>"), "<b></b>")

    def test_join(self):
        result = Markup("<br>").join(["<", Markup("<i>")])
        self.assertIsInstance(result, Markup)
        self.assertEqual(result, "&lt;<br><i>")
        self.assertEqual(Markup(", ").join(["a&b", "c"]), "a&amp;b, c")


class AutoescapeRenderingTests(unittest.TestCase):

    def test_plain_values(self):
        self.assertEqual(render("{{ '<' }}", autoescape=True), "&lt;")
        self.assertEqual(render("<input value='{{ v }}'>", autoescape=True, v="O'Neil & co"),
                         "<input value='O&#39;Neil &amp; co'>")

    def test_safe_filter(self):
        self.assertEqual(render("{{ '<b>'|safe }}", autoescape=True), "<b>")
        self.assertEqual(render("{{ html|safe }}", autoescape=True, html="<p>a & b</p>"), "<p>a & b</p>")
        self.assertIsInstance(Environment().filters["safe"]("<b>"), Markup)

    def test_escape_filter_without_autoescape(self):
        self.assertEqual(render("{{ '<a>'|e }}"), "&lt;a&gt;")
        self.assertEqual(render("{{ x|escape }}", x="&lt;"), "&amp;lt;")

    def test_escape_filter_with_autoescape_not_twice(self):
        self.assertEqual(render("{{ '<a>'|e }}", autoescape=True), "&lt;a&gt;")
        self.assertEqual(render("{{ x|escape }}", autoescape=True, x="a & b"), "a &amp; b")
        self.assertIsInstance(Environment().filters["e"]("<"), Markup)

    def test_safe_then_escape(self):
        self.assertEqual(render("{{ '<b>'|safe|e }}", autoescape=True), "<b>")
        self.assertEqual(render("{{ '<b>'|safe|e }}"), "<b>")


if __name__ == "__main__":
    unittest.main()
