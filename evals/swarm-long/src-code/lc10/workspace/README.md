# standings

League tables from match results (Python 3, standard library only): a
results file parser, table rows with point rules and deductions, the
regulation tie-breakers, form guides, fixture lists and a fixed-width text
table.

    python3 -m standings.cli results.txt --form 5 --title "Eredivisie"

See the module docstrings in `standings/` for the details, and `ISSUES.md`
for the open issues.

## Tests

    python3 -m unittest discover -s tests -t .
