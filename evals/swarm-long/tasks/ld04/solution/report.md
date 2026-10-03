Q1: 12632.00
Q2: 6295.60
Q3: 1136.75
Q4: Assembly
Q5: 4
Q6: 8115.75
Q7: 25.76
Q8: 5300.07

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - Aug hours: latest version per entry_id, approved only, post-termination entries out
- question 2 - E017 July: rate change on 2026-07-13, BE holiday 07-21 at 2x, OT weeks by Sunday (week of Jun 29 belongs to July, week ending Aug 2 to August)
- question 3 - OT hours: hourly only, weekly >40 excluding holiday hours, weeks by Sunday
- question 4 - dept cost Sept: base + OT premium of weeks ending in Sept
- question 5 - post-termination entries: count employees, current versions only
- question 6 - project hours: case/space variants of the code, Q3 date bounds (data runs Jun 29 - Oct 4)
- question 7 - effective-dated rate lookup; exclude future hires, terminated, salaried; ignore 2026-10-01 rows
- question 8 - holiday premium: per-country holidays (BE 07-21, NL 07-01, DE 09-21; Saturday holidays)
