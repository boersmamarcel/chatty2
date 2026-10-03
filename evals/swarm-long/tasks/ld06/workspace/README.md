# Site energy metering: data dictionary

Hourly electricity readings of seven sites for September 2026. The export
holds UTC intervals from 2026-08-31T12:00Z to 2026-10-01T12:00Z so that
every site's local September is covered. Comma-separated, header row.

## Files

### readings.csv
One row per reading **version**: the meter data platform re-sends an
interval when a reading is validated, estimated or corrected.

| column | meaning |
| -- | -- |
| meter_id | see meters.csv |
| interval_start_utc | start of the one-hour interval, UTC |
| kwh | register advance in the interval, in meter units (see `multiplier`) |
| quality | `A` actual, `E` estimated (a valid substitute value), `X` failed validation |
| ingested_at | when this version was loaded, UTC |

### meters.csv
`site_id` the meter belongs to; `installed_at_utc` and `removed_at_utc`
(empty = still installed): a meter's readings count only from its
installation (inclusive) to its removal (exclusive); anything a meter sends
outside that window is not site consumption. `multiplier`: the
current-transformer ratio; consumption in kWh = `kwh` x `multiplier`. A site
can have several meters at the same time; their consumption adds up.

### sites.csv
`utc_offset_hours`: the site's fixed offset from UTC for September 2026
(local = UTC + offset). `tariff_id`: see tariffs.csv.

### tariffs.csv
Energy rates per kWh (peak and off-peak) and a standing charge per day.

## Business definitions

1. **Current version.** For each (`meter_id`, `interval_start_utc`) only the
   row with the latest `ingested_at` counts.
2. **Valid reading**: current quality `A` or `E`. `X` readings count as no
   data (zero consumption, and the hour has no valid reading).
3. **Local time.** An interval belongs to the site-local date and hour of its
   start (UTC + the site's offset). "September" = local September 1-30.
4. **Peak** = intervals starting at local 07:00 up to and including 22:00 on
   Monday to Friday; everything else (nights, Saturday, Sunday) is off-peak.
5. **Energy bill** of a site for a month = peak kWh x peak rate + off-peak kWh
   x off-peak rate + standing charge x days in the month.
6. **Site hour**: the site's consumption in one local hour = the sum over its
   meters' valid readings for that interval.

Round only final answers.
