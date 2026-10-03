"""Basic rendering behaviour (expressions, statements, filters, includes)."""

import unittest

from stencil import DictLoader, Environment, StrictUndefined, TemplateSyntaxError, UndefinedError


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class ExpressionTests(unittest.TestCase):

    def test_variables_and_attributes(self):
        self.assertEqual(render("Hello {{ user.name }}!", user={"name": "Ada"}), "Hello Ada!")
        self.assertEqual(render("{{ items[1] }}{{ items.0 }}", items=["a", "b"]), "ba")

    def test_missing_values_print_empty(self):
        self.assertEqual(render("[{{ nope }}][{{ nope.deeper }}]"), "[][]")

    def test_arithmetic_and_precedence(self):
        self.assertEqual(render("{{ 1 + 2 * 3 }} {{ (1 + 2) * 3 }} {{ 7 // 2 }} {{ 7 % 4 }}"), "7 9 3 3")
        self.assertEqual(render("{{ 'n=' ~ 1 + 2 }}"), "n=3")

    def test_comparisons_and_logic(self):
        self.assertEqual(render("{{ 1 < 2 < 3 }} {{ 'a' in 'cat' }} {{ 2 not in [1, 3] }}"), "True True True")
        self.assertEqual(render("{{ none or 'x' }} {{ not true and false }}"), "x False")

    def test_filters_and_tests(self):
        self.assertEqual(render("{{ name|trim|upper }}", name="  bob "), "BOB")
        self.assertEqual(render("{{ xs|join(', ') }}", xs=[1, 2, 3]), "1, 2, 3")
        self.assertEqual(render("{{ text|truncate(9) }}", text="hello big world"), "hello...")
        self.assertEqual(render("{{ x|default('n/a') }}"), "n/a")
        self.assertEqual(render("{% if 6 is divisibleby(3) %}yes{% endif %}"), "yes")

    def test_strict_undefined(self):
        env = Environment(undefined=StrictUndefined)
        with self.assertRaises(UndefinedError):
            env.from_string("{{ missing }}").render()


class StatementTests(unittest.TestCase):

    def test_if_elif_else(self):
        src = "{% if n > 1 %}many{% elif n == 1 %}one{% else %}none{% endif %}"
        self.assertEqual([render(src, n=n) for n in (0, 1, 5)], ["none", "one", "many"])

    def test_for_with_else_and_unpacking(self):
        self.assertEqual(render("{% for k, v in d|dictsort %}{{ k }}={{ v }};{% endfor %}",
                                d={"b": 2, "a": 1}), "a=1;b=2;")
        self.assertEqual(render("{% for x in [] %}x{% else %}empty{% endfor %}"), "empty")

    def test_loop_index(self):
        self.assertEqual(render("{% for x in 'ab' %}{{ loop.index }}{{ x }}{% endfor %}"), "1a2b")

    def test_set_and_with(self):
        self.assertEqual(render("{% set a = 2 %}{% with b = a * 3 %}{{ a }}{{ b }}{% endwith %}{{ b }}"), "26")

    def test_filter_block(self):
        self.assertEqual(render("{% filter upper %}hi {{ who }}{% endfilter %}", who="you"), "HI YOU")

    def test_include(self):
        env = Environment(loader=DictLoader({
            "page.txt": "[{% include 'part.txt' %}]",
            "part.txt": "part for {{ who }}",
        }))
        self.assertEqual(env.get_template("page.txt").render(who="me"), "[part for me]")

    def test_comments_are_dropped(self):
        self.assertEqual(render("a{# note #}b"), "ab")

    def test_unknown_tag_is_a_syntax_error(self):
        with self.assertRaises(TemplateSyntaxError):
            render("{% frob %}")


if __name__ == "__main__":
    unittest.main()
