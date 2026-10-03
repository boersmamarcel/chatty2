# Open issues

Five open issues, reported by the league office. They are independent of
each other. Each lists its acceptance criteria; the behaviour described there
is what will be checked, including every edge case listed.

---

## Issue 1: results files with club numbers or postponed matches are rejected

Reported by: data desk

The results file format (see the `standings.results` docstring) allows
digits in team names and `P-P` for a postponed match, but both lines below
are rejected with a `ResultError`:

    2026-08-20  Schalke 04 2-1 FC 1912 Rovers
    2026-08-21  NAC Breda P-P Sparta Rotterdam

Acceptance (`standings.results.parse_results`):

- Team names may contain digits (as well as letters, spaces, dots,
  apostrophes and hyphens): the first line gives home `"Schalke 04"`, away
  `"FC 1912 Rovers"`, score 2-1. `"1. FC Union 0-0 Hertha"` gives home
  `"1. FC Union"`.
- A postponed match (`P-P`, also lower-case `p-p`) is skipped: it is not in
  the returned list and it is not an error. It must still have a valid date
  and two team names; otherwise it is an error like any malformed line.
- Runs of whitespace inside a name still count as one space; malformed
  lines still raise `ResultError` with `"line <n>: ..."`.

## Issue 2: teams level on points are not separated by head-to-head

Reported by: competition manager

The regulations (tie-break step 4 in the `standings.tiebreak` docstring)
put a team that beat its rival above it when they are level on points, goal
difference and goals scored. The table currently falls back to alphabetical
order straight away. Example:

    2026-09-01  Beta 2-1 Alpha
    2026-09-08  Gamma 2-1 Beta
    2026-09-15  Alpha 2-1 Gamma
    2026-09-22  Gamma 0-0 Delta

Alpha and Beta both have 3 points, goal difference 0 and 3 goals; Beta beat
Alpha, so the order must be Gamma, Beta, Alpha, Delta.

Acceptance (`standings.tiebreak.rank(rows, matches, rules=None)`):

- Teams level on points, goal difference and goals scored form a group;
  the group is ordered by the points earned in the matches between the
  group's teams only (`head_to_head`, using `rules`), descending, then by
  the goal difference in those matches, descending, then by name. Teams
  outside the group and matches against them do not count for it.
- Groups of three or more teams use the same mini-table. Steps 1-3 and the
  name order are unchanged.

## Issue 3: points deductions are not applied

Reported by: competition manager

The disciplinary committee deducted 6 points from Vitesse, but
`build_table(matches, deductions={"Vitesse": 6})` (and
`python3 -m standings.cli results.txt --deduct Vitesse=6`) still shows the
full points. The behaviour is described in the `standings.table` docstring.

Acceptance (`standings.table.build_table`):

- For every team in `deductions` the row's `deducted` is set to that
  number, so `points` is reduced by it (points may become negative).
- A team with a deduction that has not played yet still gets a row
  (played 0, points `-deduction`).
- A deduction that is negative or not an int raises `ValueError`. A
  deduction of 0 is allowed. `deductions=None` or `{}` changes nothing.

## Issue 4: form guide shows the wrong matches

Reported by: matchday programme editor

The results file is not always in date order (late corrections are appended
at the end), and the form guide takes the last matches *in the file*.
`form(team, matches, 0)` returns the whole season instead of nothing.

Acceptance (`standings.form.form(team, matches, n=5)`), as in the
`standings.form` docstring:

- The team's matches are taken in date order (oldest first; same-date
  matches in their order in `matches`) and the last `n` of them are
  listed oldest first, so the most recent result is the last letter.
- `n == 0` (or negative) gives `""`; fewer than `n` matches gives all of
  them; a team without matches gives `""`.
- `streak` and `form_table` follow from `form` and need no changes of
  their own.

## Issue 5: the printed table breaks with long names and hides the sign of the goal difference

Reported by: data desk

`Borussia Monchengladbach` pushes its line out of alignment, and a goal
difference of `5` cannot be told apart from `-5` at a glance in print
(it should read `+5`). The format is specified in the `standings.render`
docstring.

Acceptance (`standings.render.render(ranked, title=None)`):

- Team names longer than 20 characters are cut to their first 19
  characters followed by `.` (`"Borussia Monchengla."`); names of 20
  characters or fewer are unchanged.
- The goal difference is shown with a sign when it is not zero: `+5`,
  `-3`, `0`.
- Everything else is as in the docstring; for example a row with position
  1, team `Ajax`, P 3, W 2, D 1, L 0, GF 7, GA 2 and Pts 7 renders as
  `"  1  Ajax                   3   2   1   0    7    2   +5    7"`.
