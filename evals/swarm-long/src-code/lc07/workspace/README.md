# gradebook

Course grade calculation for one course section (Python 3, standard library
only): CSV import of the roster, assessments and marks, late penalties,
weighted categories with drop-lowest, letter grades and a text report.

    python3 -m gradebook.cli policy.txt roster.csv assessments.csv marks.csv

See the module docstrings in `gradebook/` for the details, and `ISSUES.md`
for the open issues.

## Tests

    python3 -m unittest discover -s tests -t .
