# Claims data mart: data dictionary and reporting rules

This is the data dictionary of the claims data mart of a mid-sized property
and casualty insurer operating in the Netherlands, Belgium and the United
Kingdom. The extract in this folder covers claims with a loss date from
1 December 2025 to 30 June 2026, together with every payment booked on those
claims up to the extract cut-off of **15 July 2026**. It is the input for the
half-year claims report of 2026. The document has four parts: the files
(part A), the reporting rules (part B), worked examples and answers to
frequent questions (part C) and the changelog (part D).

> **Precedence.** The changelog in part D amends the rules of part B. Where
> an amendment and a rule in part B disagree, the amendment governs, for
> every period in this extract, unless the amendment itself says otherwise.
> Footnotes are part of the rules.

All files are comma-separated, UTF-8, with one header row. Dates are ISO
`YYYY-MM-DD`; timestamps are ISO 8601 in UTC with a trailing `Z`. Monetary
amounts are decimal numbers with two decimals and no thousands separators.

---

## Part A. Files

### A.1 claims.csv

One row per **version** of a claim. The claims system writes a new version
whenever a claim handler changes the claim: opening it, closing it,
re-opening it, rejecting it or recording that the customer withdrew it. The
extract contains all versions, not only the latest one, and the rows are not
in version order.

| column | meaning |
| -- | -- |
| claim_id | claim identifier, stable across versions |
| policy_id | the policy the claim is made under, see policies.csv |
| loss_date | the date the loss event happened, as stated by the customer and confirmed by the handler |
| reported_at_utc | when the claim was first reported to us (first notification of loss), UTC |
| cause_code | the cause of the loss, see cause_codes.csv |
| status | `open`, `closed`, `rejected` (we declined cover), `withdrawn` (the customer withdrew the claim) |
| version_ts | when this version was written, UTC |

The columns `policy_id`, `loss_date`, `reported_at_utc` and `cause_code` do
not change between the versions of one claim in this extract; `status` does.

### A.2 claim_payments.csv

One row per payment booking on a claim. Bookings are never edited or
deleted; a wrong payment is cancelled by a **reversal** booking.

| column | meaning |
| -- | -- |
| payment_id | booking identifier |
| claim_id | the claim, see claims.csv |
| paid_date | booking date of the payment |
| payment_type | `indemnity` (paid to the customer or a repairer for the loss itself), `expense` (loss adjusters, lawyers, experts: the cost of handling the claim), `recovery` (money we received back: salvage, or recovered from a third party) |
| amount | the amount, always positive, in the **policy's currency** (see policies.csv) |
| reverses_payment_id | empty for an ordinary booking; for a reversal booking, the `payment_id` of the booking it cancels |

A reversal carries the same `payment_type` and the same amount as the
booking it cancels. After a reversal the handler may book a corrected
payment under a new `payment_id`; that new booking is an ordinary payment.

### A.3 policies.csv

| column | meaning |
| -- | -- |
| policy_id | policy identifier |
| product_code | the product sold, see product_line_map.csv |
| country | NL, BE or GB |
| currency | EUR, or GBP for GB policies; all bookings on the policy's claims are in this currency |
| inception_date | first day of cover |
| expiry_date | last day of cover (inclusive). Policies cancelled early carry the cancellation date here |
| deductible | the policy excess in policy currency, for information only: indemnity bookings are already net of it |
| is_internal | 1 for policies held by staff test accounts of the claims system (see B.2) |

### A.4 product_line_map.csv

Maps a product to a **line of business** for reporting. The mapping is
effective-dated because the reporting structure was reorganised on
1 April 2026: the fleet motor product moved to the new commercial motor line
and the premium home product got its own line. Each row is valid from
`valid_from` to `valid_to`, both inclusive; an empty `valid_to` means the row
is still valid.

| column | meaning |
| -- | -- |
| product_code | product, as in policies.csv |
| line_of_business | `motor`, `commercial_motor`, `home`, `home_premium`, `travel` |
| valid_from | first day the row applies |
| valid_to | last day the row applies, empty = open-ended |

### A.5 cause_codes.csv

| column | meaning |
| -- | -- |
| cause_code | code as used in claims.csv |
| description | readable name |
| applies_to | the lines the code is normally used for (informational) |
| cat_window_start, cat_window_end | for natural-catastrophe codes: the first and last day (inclusive) of the declared catastrophe event; empty for other codes |

### A.6 fx_rates.csv

Monthly average rates. `eur_per_unit` is the number of euros for one unit of
`currency` in `month` (`YYYY-MM`). EUR rows are 1.

---

## Part B. Reporting rules

### B.1 Current version of a claim

Only the latest version of each claim counts: the row with the greatest
`version_ts` for that `claim_id`. Earlier versions are history and are
ignored in every figure. The order of the rows in the file means nothing.

### B.2 Claims in scope

1. A claim whose current status is `withdrawn` is out of scope for every
   figure: it is not counted, and none of its payments count, including any
   expense bookings made before the withdrawal.
2. Rejected claims are in scope. They count as reported claims, and the
   expense bookings on them count in paid amounts.
3. Claims under internal policies (`is_internal` = 1) are test data and are
   out of scope for every figure.
4. A claim is **out of cover** when its loss date is before the policy's
   inception date or after its expiry date. Out-of-cover claims are out of
   scope for every figure except where a question explicitly asks about
   out-of-cover claims; such a question still applies rules 1 and 3.

### B.3 Reporting period of a claim

Claims are reported in the month of their **reported_at_utc** timestamp
(the UTC calendar month of the first notification of loss)[^basis]. A
quarter or half-year is the union of its months.

### B.4 Line of business

The line of business of a claim is found through its policy's
`product_code` in product_line_map.csv, using the mapping row that is valid
on the claim's **loss date**. It is not the line valid today, and not the
line valid on the reporting date.

### B.5 Payments and net paid

1. A reversal booking and the booking it reverses both drop out of every
   figure (together they are zero). A corrected re-booking after a reversal
   counts normally.
2. **Net paid** of a claim = indemnity + expense − recovery, over its
   remaining bookings up to the cut-off, converted to euros.
3. Each booking converts to euros at the rate of the month of its own
   `paid_date`[^fx].

### B.6 Catastrophe claims

A claim is a **catastrophe (CAT) claim** when its cause code has a
catastrophe window and its loss date lies inside that window (both ends
inclusive). A storm, flood or hail claim with a loss date outside the window
is an ordinary (attritional) claim. "Non-CAT" means all other claims.

### B.7 Large losses

A claim is a **large loss** when its net paid exceeds EUR 50,000.00 (strictly
greater). See part D.

### B.8 Reporting delay

The reporting delay of a claim in days = the UTC calendar date of
`reported_at_utc` minus the loss date. A claim reported on its loss date has
a delay of 0.

### B.9 Status questions

"Closed claims" are claims whose current status is `closed`. "Open claims"
are claims whose current status is `open`; a re-opened claim is open.

[^basis]: The reported-date basis was the regulatory reporting convention
until the 2026 reorganisation. Read part D before using it.

[^fx]: Not the rate of the loss month and not the rate of the reporting
month. A claim paid over several months therefore mixes rates.

---

## Part C. Worked examples and frequent questions

**Example 1 (versions).** Claim CL9999991 has two rows: `open` written on
2026-03-02 and `closed` written on 2026-04-10. Its current status is
`closed`. If the rows were the other way round in the file, the answer would
be the same: only `version_ts` decides.

**Example 2 (reversal).** Claim CL9999992 has indemnity bookings PAY1 of
1,000.00 on 2026-03-05 and PAY2 of 1,000.00 on 2026-03-09 with
`reverses_payment_id` = PAY1, and then PAY3 of 900.00 on 2026-03-20. Its
indemnity is 900.00: PAY1 and PAY2 cancel out. Counting PAY2 as a second
payment would give 2,900.00; subtracting it without removing PAY1 would give
900.00 too, but only when the reversal carries the same type and amount,
which this extract guarantees.

**Example 3 (currency).** A GB claim has an indemnity of GBP 2,000.00 paid in
February and an expense of GBP 100.00 paid in April. Its net paid is
2,000.00 x (February GBP rate) + 100.00 x (April GBP rate).

**Example 4 (mapping).** A fleet motor policy (MOT-FLEET) claim with a loss on
2026-03-30 is a `motor` claim, even if it was reported in April and even
though the product maps to `commercial_motor` today. The same policy's claim
with a loss on 2026-04-02 is `commercial_motor`.

**Example 5 (catastrophe).** A STORM claim with a loss date of 2026-02-16 is a
CAT claim (storm Ines, window 14 to 18 February). A STORM claim with a loss
date of 2026-03-03 is not.

**Q: Do expense bookings count towards the large-loss test?** Yes: the test
is on net paid, which includes expenses and is reduced by recoveries.

**Q: Do I need the deductible?** No. Indemnity bookings are already net of
the deductible. The column is for information only.

**Q: Some claims have no payments. Are they in scope?** Yes. Their net paid is
zero; they count in claim counts.

**Q: What about payments booked after the cut-off?** The extract contains
none. Use every booking in the file (subject to B.5).

**Q: Is an out-of-cover claim's loss still a CAT claim?** It is out of scope
(B.2 rule 4) unless the question is about out-of-cover claims.

**Q: Which date decides whether a claim is in "Q2 2026"?** The reporting
period of B.3 as amended in part D.

**Q: What does "average" mean?** The arithmetic mean over the claims in
scope of the question.

---

## Part D. Changelog (amendments to part B)

**A1 (2026-01-15). Catastrophe windows.** The storm Ines window was set to
14-18 February 2026 after the event was declared. Later events are added to
cause_codes.csv when they are declared (flood 20-24 May, hail 10-11 June).

**A2 (2026-05-01). Reporting basis.** From the 2026 reorganisation onwards,
all claims figures are reported by **accident month**: the month of the
claim's loss date, not the month of `reported_at_utc`. This replaces rule
B.3 and applies to all periods in this extract, including the months before
May 2026 (prior reports have been restated). Quarters and half-years are the
unions of their accident months.

**A3 (2026-05-01). Large-loss threshold.** The large-loss threshold of rule
B.7 is lowered from EUR 50,000.00 to **EUR 25,000.00** (net paid strictly
greater than the threshold). The new threshold applies to every claim in
this extract, whatever its loss date.

**A4 (2026-06-01). Clarification of B.5.** Recoveries are always reported
in the period of the claim they relate to (the claim's accident month), even
when received months later, and are converted at the rate of their own
paid month like every other booking.

**A5 (2026-06-20). No change to B.2.** Rejected claims remain in scope after
the review of June 2026 (see B.2 rule 2).

---

## Part E. Data-quality notes and glossary

### E.1 Known data-quality issues in this extract

1. **Duplicate-looking claims.** Two claims of the same policy with the same
   loss date and cause are separate claims when their `claim_id` differs
   (for example a vehicle and its trailer). Do not merge them.
2. **Version noise.** Roughly four in ten claims have more than one version.
   Earlier versions frequently show `open` for a claim that is now closed,
   or `closed` for a claim that was later re-opened. Counting rows instead of
   claims, or using the first version, overstates and misclassifies claims.
3. **Withdrawn claims with costs.** Some withdrawn claims carry an expense
   booking for an adjuster visit made before the customer withdrew. Under
   B.2 rule 1 these bookings are excluded together with the claim.
4. **Early-cancelled policies.** About one policy in twelve was cancelled
   before its anniversary; its `expiry_date` is the cancellation date. A
   loss after that date is out of cover even though a full-year policy would
   have covered it.
5. **Late reporting.** Home claims are typically reported a week or more
   after the loss; motor claims within a few days. A claim reported in July
   2026 can have a loss date in June 2026 and is then part of the June
   accident month.
6. **Payments in GBP.** All bookings of a GB policy are in GBP, including
   expenses paid to Dutch adjusters. Convert them with B.5 rule 3; there is
   no currency column on the booking itself.
7. **Reversal timing.** A reversal can be booked in a later month than the
   booking it cancels. Both drop out regardless of their months.

### E.2 Glossary

- **Accident month / accident period.** The month (or the union of months)
  of the loss date. Since amendment A2 this is the reporting period of
  every claim figure.
- **Attritional claim.** Any claim that is not a CAT claim.
- **Claim in scope.** A claim that survives every rule of B.2: current status
  not withdrawn, policy not internal, loss date within the policy's cover.
- **First notification of loss (FNOL).** The moment the claim was first
  reported; `reported_at_utc`.
- **Indemnity.** Payments for the loss itself, after the deductible.
- **Line of business (LoB).** The reporting segment of B.4.
- **Net paid.** B.5 rule 2.
- **Recovery ratio.** Recoveries divided by indemnity over the same set of
  claims, in euros, as a percentage.
- **Reporting delay.** B.8.

### E.3 Reconciliation totals

Finance reconciles the extract against the general ledger on gross bookings
(every row of claim_payments.csv, reversals included, in policy currency).
Those control totals are **not** reporting figures and must not be used to
answer reporting questions: the reporting rules above remove reversals,
withdrawn and internal claims and convert currencies, which the ledger
control does not.

### E.4 Checklist

Before reporting a figure, check that you have: kept only the current
version of each claim; dropped withdrawn claims, internal policies and
out-of-cover claims; assigned the accident month from the loss date (A2);
mapped the line of business on the loss date; removed reversal pairs;
converted each booking at its own paid month; tested the catastrophe window
on the loss date; and used the amended large-loss threshold (A3).
