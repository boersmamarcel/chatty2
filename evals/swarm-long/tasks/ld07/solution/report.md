Q1: 177
Q2: 913782.08
Q3: 18
Q4: 3412.46
Q5: 1.60
Q6: 73
Q7: 8.61
Q8: 137275.13

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - count: current version, withdrawn/internal/out-of-cover out, accident month (A2), mapping on loss date
- question 2 - net paid March: reversal pairs out, FX by paid month, recoveries subtracted, accident month, withdrawn expenses out
- question 3 - large losses: amended 25k threshold (A3), net paid incl expenses, FX by paid month
- question 4 - motor avg: MOT-FLEET is motor before 04-01, CAT by window, current status closed
- question 5 - recovery ratio: mapping effective from 04-01, reversals, recoveries paid later count (A4)
- question 6 - out of cover: inclusive policy dates incl early cancellations, withdrawn/internal still excluded
- question 7 - delay: HM-PLUS is home only before 04-01, CAT window exclusion, accident month
- question 8 - CAT expenses: window test on loss date, rejected claims' expenses count, withdrawn out
