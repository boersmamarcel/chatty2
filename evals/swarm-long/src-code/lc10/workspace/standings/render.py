"""The fixed-width text table.

    Pos  Team                   P   W   D   L   GF   GA   GD  Pts
      1  Ajax                   3   2   1   0    7    2   +5    7
      2  PSV                    3   1   1   1    4    4    0    4

Columns: position (width 3, right-aligned), two spaces, team name (width
20, left-aligned), then P, W, D, L (width 3 each) and GF, GA, GD, Pts
(width 4 each), every number right-aligned and preceded by one space. Team
names longer than 20 characters are cut to 19 characters followed by a
`.`. Goal difference is signed: `+5`, `0`, `-3`. Lines have no trailing
whitespace and are joined with newlines (no final newline).
"""

NAME_WIDTH = 20

HEADER = ("Pos", "Team", "P", "W", "D", "L", "GF", "GA", "GD", "Pts")


def _line(cells):
    pos, team, p, w, d, l, gf, ga, gd, pts = cells
    text = "%3s  %-*s %3s %3s %3s %3s %4s %4s %4s %4s" % (
        pos, NAME_WIDTH, team, p, w, d, l, gf, ga, gd, pts)
    return text.rstrip()


def render(ranked, title=None):
    """Text table of rows already in table order."""
    lines = []
    if title:
        lines.append(title)
        lines.append("")
    lines.append(_line(HEADER))
    for position, row in enumerate(ranked, 1):
        lines.append(_line((position, row.team, row.played, row.won, row.drawn,
                            row.lost, row.goals_for, row.goals_against,
                            row.goal_diff, row.points)))
    return "\n".join(lines)


def render_csv(ranked):
    """The same table as CSV text (full team names, plain goal difference),
    one header line plus one line per row, ending with a newline."""
    lines = ["pos,team,p,w,d,l,gf,ga,gd,pts"]
    for position, row in enumerate(ranked, 1):
        team = row.team.replace('"', '""')
        if "," in team or '"' in team:
            team = '"%s"' % team
        lines.append("%d,%s,%d,%d,%d,%d,%d,%d,%d,%d" % (
            position, team, row.played, row.won, row.drawn, row.lost,
            row.goals_for, row.goals_against, row.goal_diff, row.points))
    return "\n".join(lines) + "\n"
