# Payroll costing: data dictionary

Timesheets of the operations staff for the weeks from Monday 2026-06-29 to
Sunday 2026-10-04, used for labour costing. Comma-separated, header row.

## Files

### timesheets.csv
One row per timesheet entry **version**: an entry is re-submitted under the
same `entry_id` when the employee or a manager corrects it.

| column | meaning |
| -- | -- |
| entry_id | entry identifier |
| employee_id | see employees.csv |
| work_date | the day worked |
| hours | hours worked (quarter hours) |
| project_code | project booked; codes are case-insensitive and may carry stray spaces |
| status | `approved`, `submitted` (awaiting approval) or `rejected` |
| submitted_at | when this version was submitted, UTC |

### employees.csv
`dept_id` (see departments.csv), `country` (the employee's work country, for
public holidays), `pay_type` (`hourly` or `salaried`), `hire_date`,
`termination_date` (last day of employment; empty while employed).

### pay_rates.csv
Effective-dated hourly rates (EUR). The rate in effect on a day is the row
of that employee with the latest `effective_from` on or before that day.
Salaried staff have a costing rate too.

### departments.csv, public_holidays.csv
Department names; public holidays per country.

## Business definitions

1. **Current version.** When an `entry_id` appears more than once, only the
   row with the latest `submitted_at` counts.
2. **Payable entry.** Current status `approved`, and the work date is not
   after the employee's termination date (entries after it are data errors
   and are excluded from every figure except where a question asks for them).
3. **Base pay** of a payable entry = hours x the rate in effect on the work
   date; hours worked on a public holiday of the employee's country are paid
   double (2 x the rate). The extra 1 x on holiday hours is the **holiday
   premium**.
4. **Overtime** applies to `hourly` employees only. Per employee and per week
   (Monday to Sunday), overtime hours = payable hours of the week that are
   not holiday hours, minus 40, if positive. The **overtime premium** =
   0.5 x overtime hours x the rate in effect on that week's Sunday. A week
   belongs to the month (and quarter) of its Sunday.
5. **Labour cost** of a month = base pay of the payable entries dated in that
   month + the overtime premiums of the weeks belonging to that month.

Round only final answers.
