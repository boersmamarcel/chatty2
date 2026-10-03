import unittest

from standings.render import render
from standings.table import Row


def row(team, played, won, drawn, lost, gf, ga):
    r = Row(team)
    r.played, r.won, r.drawn, r.lost = played, won, drawn, lost
    r.goals_for, r.goals_against = gf, ga
    return r


class RenderTest(unittest.TestCase):
    def test_example(self):
        text = render([row("Ajax", 3, 2, 1, 0, 7, 2), row("PSV", 3, 1, 1, 1, 4, 4),
                       row("Borussia Monchengladbach", 3, 0, 0, 3, 1, 6)])
        self.assertEqual(text.split("\n"), [
            "Pos  Team                   P   W   D   L   GF   GA   GD  Pts",
            "  1  Ajax                   3   2   1   0    7    2   +5    7",
            "  2  PSV                    3   1   1   1    4    4    0    4",
            "  3  Borussia Monchengla.   3   0   0   3    1    6   -5    0",
        ])

    def test_name_lengths(self):
        twenty = "A" * 20
        lines = render([row(twenty, 0, 0, 0, 0, 0, 0), row("B" * 21, 0, 0, 0, 0, 0, 0)]).split("\n")
        self.assertEqual(lines[1][5:26], twenty + " ")
        self.assertEqual(lines[2][5:26], "B" * 19 + ". ")
        self.assertEqual(len(lines[1]), len(lines[2]))

    def test_title_and_big_numbers(self):
        text = render([row("Ajax", 34, 30, 2, 2, 120, 15)], title="Eredivisie")
        lines = text.split("\n")
        self.assertEqual(lines[0], "Eredivisie")
        self.assertEqual(lines[1], "")
        self.assertEqual(lines[3], "  1  Ajax                  34  30   2   2  120   15 +105   92")
        self.assertFalse(text.endswith("\n"))


if __name__ == "__main__":
    unittest.main()
