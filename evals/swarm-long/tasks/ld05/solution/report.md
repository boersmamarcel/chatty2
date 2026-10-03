Q1: 14.50
Q2: 59550.00
Q3: P11
Q4: 71
Q5: 24.0
Q6: K3
Q7: 47
Q8: 16536.00

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - Q2 no-show rate: late cancels (<24h, exactly 24h not late) count as no-shows; other cancels out of the denominator
- question 2 - March revenue: tariff by patient class (uninsured = S), no-show fee incl late cancels, re-sent no_show->completed
- question 3 - top clinician May: current versions only
- question 4 - 65+ with >=3 visits: exact age on 2026-06-30 (birthday not yet reached)
- question 5 - median lead time: date difference, completed new_patient only
- question 6 - clinic no-show rate over H1 with late cancels
- question 7 - repeat no-shows: late cancels included, corrected no_show rows superseded
- question 8 - class S revenue: uninsured patients are S; April fee schedule; no-show fees included
