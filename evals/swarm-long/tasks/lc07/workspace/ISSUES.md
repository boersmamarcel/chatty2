# Open issues

Five open issues, reported by the teaching staff. They are independent of
each other. Each lists its acceptance criteria; the behaviour described there
is what will be checked, including every edge case listed.

---

## Issue 1: students exactly on a grade boundary get the lower letter

Reported by: course coordinator

A student with a course percentage of exactly 90.00 got a B+, and one with
89.96 (shown as 89.96, but the syllabus says grades are decided on the
percentage rounded to one decimal, i.e. 90.0) also got a B+.

Acceptance (`gradebook.grading.letter_for(pct)`):

- The percentage is first rounded half-up to one decimal (89.95 -> 90.0,
  89.94 -> 89.9); the letter is the first entry of `SCALE` whose lower
  bound is less than or equal to the rounded value (bounds are inclusive):
  90.0 -> `"A-"`, 89.95 -> `"A-"`, 89.94 -> `"B+"`, 93.0 -> `"A"`,
  59.95 -> `"D"`, 59.94 -> `"F"`, 0 -> `"F"`.
- Percentages above 100 (extra credit) are `"A"`.
- A negative percentage raises `GradingError`.

## Issue 2: drop-lowest drops the wrong assessments

Reported by: quiz coordinator

Quizzes have different maximum points (some are out of 5, some out of 20).
`drop_lowest` drops the result with the fewest *points*, so a 4/5 quiz
(80 %) is dropped instead of a 9/20 (45 %). With `drop=2` and only two
quizzes graded for a student, both are dropped and the category disappears.
The function also returns the entries re-ordered.

Acceptance (`gradebook.policy.drop_lowest(entries, n)`; entries are
`(Assessment, points)` pairs):

- The weakest entry is the one with the lowest `points / max_points`. Ties
  are broken by due date: the entry with the *earlier* `due` is dropped
  first; an assessment with `due` None counts as due after every dated one;
  remaining ties by assessment `key` ascending.
- At least one entry is always kept: with `n >= len(entries)` all but the
  single strongest entry are dropped (the strongest by the same ordering).
  An empty list returns an empty list.
- The kept entries are returned in their original order (a new list).
- `n == 0` returns a copy of the list; a negative `n` raises `PolicyError`.

## Issue 3: late penalties are computed on the wrong base

Reported by: course coordinator

Work handed in 2 hours late was not penalised at all, a submission 9 days
late lost 90 % of its points, and the penalty is a percentage of the points
*earned* instead of the maximum points. Extensions granted to a student
(`extension_days` in marks.csv) are ignored.

Acceptance (`gradebook.late`), following syllabus section 4.2 (see the
module docstring):

- `days_late(due, submitted)` counts every started day: 1 minute late is
  1, exactly 24 hours late is 1, 24 hours and 1 minute late is 2. On time
  or early is 0; `due` or `submitted` None is 0.
- `apply_late(points, max_points, due, submitted, extension_days=0)`
  moves the deadline `extension_days` whole days later (`effective_due`),
  then subtracts 10 % of `max_points` per day late, for at most 5 days
  (50 % of `max_points`). The result is never below 0.0. `None` and
  `EXCUSED` are returned unchanged.
- Example: max 20, 15 points, 1 h late -> 13.0; 3 days and 1 h late ->
  7.0; 9 days late -> 5.0; 2 points, 2 days late -> 0.0; 25 h after the
  deadline with `extension_days=1` -> 13.0.

## Issue 4: marks.csv with blank, excused or comma-decimal points cannot be loaded

Reported by: teaching assistants

The marks export from the LMS writes an empty `points` cell for work that
was never handed in, `EX` for excused work and, from the Dutch locale,
`7,5` for 7.5 points. All three make the import fail. When an import fails,
the line number in the message is one less than the line in the file.

Acceptance (`gradebook.loader`):

- `parse_points(text)` returns `None` for an empty or whitespace-only cell
  (missing work), `EXCUSED` (from `gradebook.models`) for `EX` in any case
  with optional surrounding whitespace, and a float otherwise; a single
  decimal comma is accepted (`"7,5"` -> 7.5, `" 12 "` -> 12.0).
- `parse_points` raises `LoaderError` for anything else, including a
  negative number (`"-1"`), `"abc"`, `"1,2,3"` and `"1.5,5"`.
- `load_marks` keeps those values on the returned `Mark` objects (missing
  -> `points is None`, excused -> `points is EXCUSED`).
- Every `load_marks` error message is `"marks line <n>: <reason>"`, where
  `<n>` is the line in the file: the header is line 1, so the first data row
  is line 2.

## Issue 5: class statistics and ranks in the report are wrong

Reported by: head of department

With an even number of students the reported median is not the median. The
standard deviation is the population one, while the department reports the
sample standard deviation. Two students with the same percentage get
different ranks, and the report crashes with `ZeroDivisionError` on an
empty section instead of a clear error.

Acceptance (`gradebook.report`):

- `median(values)`: for an even count, the mean of the two middle values
  (`[1, 2, 3, 4]` -> 2.5); odd counts are unchanged.
- `stdev(values)`: the sample standard deviation (divide by n - 1); 0.0 for
  fewer than two values (`[2, 4, 4, 4, 5, 5, 7, 9]` -> 2.138...).
- `class_stats([])` raises `ValueError`.
- `rank_rows(rows)` uses standard competition ranking: equal percentages
  share a rank and the next rank skips (`1, 2, 2, 4`). Order is unchanged:
  percentage descending, then name ascending.
