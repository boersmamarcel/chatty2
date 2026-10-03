# Outpatient appointments: data dictionary

Appointments of four outpatient clinics with slots from 2026-01-01 to
2026-06-30, exported on 2026-07-01. Comma-separated, header row. All local
times (`slot_start`, `booked_at`, `cancelled_at`) are clinic local time,
format `YYYY-MM-DD HH:MM`.

## Files

### appointments.csv
One row per appointment **version**: the scheduling system re-sends an
appointment whenever its status changes.

| column | meaning |
| -- | -- |
| appt_id | appointment identifier |
| patient_id | see patients.csv |
| clinician_id | see clinicians.csv |
| clinic_id | where the appointment takes place |
| visit_type | `new_patient`, `follow_up`, `procedure`, `telehealth` |
| slot_start | appointment start, local |
| booked_at | when the appointment was booked, local |
| status | `scheduled`, `completed`, `no_show` or `cancelled` |
| cancelled_at | when the patient cancelled, local (cancelled only) |
| updated_at | when this version was recorded, UTC |

### patients.csv
`birth_date`, `insurer_id` (empty = uninsured), `postcode`.

### insurers.csv
`tariff_class` of each insurer: `A`, `B` or `S` (self-pay rates).

### tariffs.csv
Fee per visit type and tariff class, effective-dated: the fee for a visit is
the row with the latest `effective_from` on or before the slot date.

### clinicians.csv, clinics.csv
Clinician and clinic master data.

## Business definitions

1. **Current version.** When an `appt_id` appears more than once, only the
   row with the latest `updated_at` counts. Appointments whose current status
   is still `scheduled` were never closed out and are ignored.
2. **Late cancellation**: a cancelled appointment whose `cancelled_at` is
   less than 24 hours before `slot_start` (exactly 24 hours is not late).
   A late cancellation is treated as a **no-show** in every figure.
3. **No-show rate** = no-shows / (completed + no-shows), no-shows including
   late cancellations. Other cancellations are left out entirely.
4. **Tariff class** of a patient = the class of their insurer; uninsured
   patients are class `S`.
5. **Billed revenue** = for each completed appointment the fee of its visit
   type and the patient's tariff class in effect on the slot date, plus a
   flat no-show fee of 30.00 EUR for each no-show (including late
   cancellations), whatever the class.
6. **Age** = completed years on 2026-06-30.
7. Months and quarters refer to the slot date.

Round only final answers.
