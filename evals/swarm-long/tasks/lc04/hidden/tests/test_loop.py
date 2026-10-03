"""The ``loop`` variable of ``{% for %}``."""

import unittest

from stencil import Environment


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class LoopTests(unittest.TestCase):

    def test_comma_separated_list(self):
        src = "{% for x in xs %}{{ x }}{% if not loop.last %}, {% endif %}{% endfor %}"
        self.assertEqual(render(src, xs=["a", "b", "c"]), "a, b, c")

    def test_revindex_counts_down_to_one(self):
        src = "{% for x in xs %}{{ loop.revindex }}{% endfor %}"
        self.assertEqual(render(src, xs=["a", "b", "c"]), "321")

    def test_revindex0_counts_down_to_zero(self):
        src = "{% for x in xs %}{{ loop.revindex0 }}{% endfor %}"
        self.assertEqual(render(src, xs=["a", "b", "c"]), "210")


if __name__ == "__main__":
    unittest.main()
