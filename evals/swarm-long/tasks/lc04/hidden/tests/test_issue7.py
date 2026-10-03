"""Issue 7: {% for ... if condition %}."""

import unittest

from stencil import Environment, TemplateSyntaxError


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class LoopFilterTests(unittest.TestCase):

    def test_items_are_filtered(self):
        self.assertEqual(render("{% for x in xs if x is odd %}{{ x }}{% endfor %}", xs=range(8)), "1357")

    def test_counters_count_passing_items(self):
        src = "{% for x in [1, 5, 2, 7] if x > 1 %}{{ loop.index }}/{{ loop.length }} {% endfor %}"
        self.assertEqual(render(src), "1/3 2/3 3/3 ")

    def test_index0_and_first(self):
        src = ("{% for u in users if u.active %}{{ loop.index0 }}{% if loop.first %}*{% endif %}"
               "{{ u.name }} {% endfor %}")
        users = [{"name": "a", "active": False}, {"name": "b", "active": True},
                 {"name": "c", "active": False}, {"name": "d", "active": True}]
        self.assertEqual(render(src, users=users), "0*b 1d ")

    def test_unpacking(self):
        src = "{% for k, v in pairs if v %}{{ k }}={{ v }};{% endfor %}"
        self.assertEqual(render(src, pairs=[("a", 1), ("b", 0), ("c", 3)]), "a=1;c=3;")

    def test_condition_uses_outer_variables(self):
        src = "{% for x in xs if x >= limit %}{{ x }}{% endfor %}"
        self.assertEqual(render(src, xs=[3, 8, 5, 10], limit=5), "8510")

    def test_dict_items(self):
        src = "{% for k, v in d|dictsort if v > 1 %}{{ k }}{{ loop.index }}{% endfor %}"
        self.assertEqual(render(src, d={"a": 1, "b": 2, "c": 3}), "b1c2")


class ElseTests(unittest.TestCase):

    def test_else_when_nothing_passes(self):
        src = "{% for x in xs if x > 5 %}{{ x }}{% else %}none{% endfor %}"
        self.assertEqual(render(src, xs=[1, 2]), "none")

    def test_else_on_empty(self):
        src = "{% for x in xs if x > 5 %}{{ x }}{% else %}none{% endfor %}"
        self.assertEqual(render(src, xs=[]), "none")

    def test_no_else_when_something_passes(self):
        src = "{% for x in xs if x > 5 %}{{ x }}{% else %}none{% endfor %}"
        self.assertEqual(render(src, xs=[1, 9]), "9")


class SyntaxTests(unittest.TestCase):

    def test_missing_condition(self):
        with self.assertRaises(TemplateSyntaxError):
            Environment().from_string("{% for x in xs if %}{% endfor %}")

    def test_plain_loops_unchanged(self):
        self.assertEqual(render("{% for x in xs %}{{ loop.index }}{{ x }}{% endfor %}", xs="ab"), "1a2b")


if __name__ == "__main__":
    unittest.main()
