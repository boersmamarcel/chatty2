Q1: 49146.93
Q2: 68.10
Q3: 17116.83
Q4: S05
Q5: 37
Q6: 2026-09-17
Q7: 8061.48
Q8: 508.46

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - S03: meter swap M03->M08 on 09-14 09:00Z, M08 multiplier 2, M03 ghost readings and M08 pre-install pulses excluded
- question 2 - peak share: local hours per site offset, 22:00 hour inclusive, weekdays only, multipliers
- question 3 - S05 bill: CT multiplier 40, tariff T3 peak/off-peak split, 30 days standing charge
- question 4 - max site hour: multiplier decides (S05 raw readings are small); X spikes excluded
- question 5 - S06 missing hours: UTC-4 window of 720 local hours, gaps plus current-X intervals (X later replaced by E counts as valid)
- question 6 - peak day: per-site local dates, all meters with multipliers
- question 7 - estimated kWh: E rows later replaced by A do not count; X replaced by E does
- question 8 - S01 weekend average: local weekend dates (8 days), UTC+2 boundaries
