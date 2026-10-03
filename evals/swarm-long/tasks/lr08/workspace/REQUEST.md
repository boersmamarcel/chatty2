# Request

Tidewater Pay suffered a payment-processing outage (incident INC-2291) in July 2026. The reliability lead needs a consolidated post-mortem brief assembled from the pile of drafts, chat exports, timelines, policies and meeting notes in docs/.

Write `brief.md` in the workspace root answering the 8 numbered questions below, in order. Give each answer explicitly (the number, date or name asked for, with units) and cite the source file in square brackets right after it, for example `[policy-v3.md]`. Cite only the files under `docs/`; use the corpus only, no web and no URLs. The documents contain superseded versions, similar-looking material for other entities or years, and places where sources disagree: state the answer that the rules in the documents make authoritative.

## Questions

1. At what time (UTC) did customer impact of INC-2291 begin?
2. How long was the customer impact in total, in minutes, according to the final timeline?
3. What was the root cause of the outage?
4. How many payment transactions failed in total across the EU and US regions?
5. What total SLA credit (in EUR) is owed to the affected customers under the credit policy in force on the incident date?
6. By what date is action item AP-4 (certificate rotation and expiry monitoring) due, per the latest action-item register?
7. Which alert channel did the certificate-expiry warning go to, so that nobody saw it?
8. What severity does the incident carry in the final classification under the severity policy in force?
