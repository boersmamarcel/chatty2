"""Issue 1: loop variables."""

import unittest

from stencil import Environment
from stencil.runtime import LoopContext


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


ROW = ("{{ loop.index }},{{ loop.index0 }},{{ loop.revindex }},{{ loop.revindex0 }},"
       "{{ loop.first }},{{ loop.last }},{{ loop.length }};")


class CountersTests(unittest.TestCase):

    def test_all_counters_on_a_list(self):
        out = render("{% for x in xs %}" + ROW + "{% endfor %}", xs=["a", "b", "c"])
        self.assertEqual(out, "1,0,3,2,True,False,3;2,1,2,1,False,False,3;3,2,1,0,False,True,3;")

    def test_single_item_is_first_and_last(self):
        out = render("{% for x in xs %}" + ROW + "{% endfor %}", xs=["only"])
        self.assertEqual(out, "1,0,1,0,True,True,1;")

    def test_comma_list(self):
        src = "{% for x in xs %}{{ x }}{% if not loop.last %}, {% endif %}{% endfor %}"
        self.assertEqual(render(src, xs=[1, 2, 3, 4]), "1, 2, 3, 4")

    def test_generator(self):
        out = render("{% for x in xs %}" + ROW + "{% endfor %}", xs=(c for c in "ab"))
        self.assertEqual(out, "1,0,2,1,True,False,2;2,1,1,0,False,True,2;")

    def test_iterator(self):
        src = "{% for x in xs %}{{ x }}{% if loop.last %}!{% endif %}{% endfor %}"
        self.assertEqual(render(src, xs=iter([1, 2])), "12!")

    def test_loopcontext_directly(self):
        loop = LoopContext(n for n in range(4))
        seen = [(item, loop.revindex, loop.revindex0, loop.last) for item in loop]
        self.assertEqual(seen, [(0, 4, 3, False), (1, 3, 2, False), (2, 2, 1, False), (3, 1, 0, True)])
        self.assertEqual(loop.length, 4)


class NeighbourTests(unittest.TestCase):

    def test_previtem_and_nextitem(self):
        src = "{% for x in xs %}[{{ loop.previtem }}<{{ x }}>{{ loop.nextitem }}]{% endfor %}"
        self.assertEqual(render(src, xs=[1, 2, 3]), "[<1>2][1<2>3][2<3>]")

    def test_edges_are_undefined(self):
        src = ("{% for x in xs %}{{ loop.previtem is defined }}/{{ loop.nextitem is defined }};"
               "{% endfor %}")
        self.assertEqual(render(src, xs="abc"), "False/True;True/True;True/False;")

    def test_neighbours_with_generator(self):
        src = "{% for x in xs %}{{ loop.nextitem }}{% endfor %}"
        self.assertEqual(render(src, xs=(n * 10 for n in range(3))), "1020")


class CycleTests(unittest.TestCase):

    def test_cycle_two_values(self):
        src = '{% for x in "abc" %}{{ loop.cycle("odd", "even") }} {% endfor %}'
        self.assertEqual(render(src), "odd even odd ")

    def test_cycle_three_values(self):
        src = "{% for x in xs %}{{ loop.cycle('r', 'g', 'b') }}{% endfor %}"
        self.assertEqual(render(src, xs=range(7)), "rgbrgbr")


class NestingTests(unittest.TestCase):

    def test_depth(self):
        src = "{% for a in [1, 2] %}{{ loop.depth }}{% for b in [1] %}{{ loop.depth }}{{ loop.depth0 }}{% endfor %}{% endfor %}"
        self.assertEqual(render(src), "121121")

    def test_outer_loop_restored_after_inner(self):
        src = ("{% for a in xs %}{% for b in ys %}{% endfor %}"
               "{{ loop.index }}{{ loop.revindex }}{{ loop.last }} {% endfor %}")
        self.assertEqual(render(src, xs="ab", ys=[1, 2, 3]), "12False 21True ")


if __name__ == "__main__":
    unittest.main()
