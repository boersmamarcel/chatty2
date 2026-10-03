"""lr02: incident post-mortem (Tidewater Pay, INC-2291)."""

TITLE = "Incident post-mortem: INC-2291 payment outage"
INTRO = ("Tidewater Pay suffered a payment-processing outage (incident INC-2291) in July 2026. The reliability "
         "lead needs a consolidated post-mortem brief assembled from the pile of drafts, chat exports, "
         "timelines, policies and meeting notes in docs/.")
QUESTIONS = [
    "At what time (UTC) did customer impact of INC-2291 begin?",
    "How long was the customer impact in total, in minutes, according to the final timeline?",
    "What was the root cause of the outage?",
    "How many payment transactions failed in total across the EU and US regions?",
    "What total SLA credit (in EUR) is owed to the affected customers under the credit policy in force on the incident date?",
    "By what date is action item AP-4 (certificate rotation and expiry monitoring) due, per the latest action-item register?",
    "Which alert channel did the certificate-expiry warning go to, so that nobody saw it?",
    "What severity does the incident carry in the final classification under the severity policy in force?",
]

DOCS = [
 dict(name="postmortem-draft-v1.md", title="Post-mortem draft v1 (SUPERSEDED)", core="""Status: DRAFT v1, written 2026-07-16 within 24 hours of resolution. SUPERSEDED by the final post-mortem (v3).

@@PAD@@

## Summary
Customer impact started at about 16:07 and ended at 15:31 (times as shown in the on-call pager, which displays Central European Summer Time in this draft; the two times were not converted consistently). Root cause suspected: database failover in the primary payments cluster. Severity: SEV-2.

## Counts
Failed transactions: EU 18,420 (US figure not yet available).
@@PAD@@
"""),
 dict(name="postmortem-final-v3.md", title="Post-mortem (final, v3)", core="""Status: FINAL v3, approved by the reliability council on 2026-07-28. This version supersedes drafts v1 and v2 and the chat-export narrative; where they disagree, this document governs (rule: "final post-mortem overrides chat exports and drafts").

@@PAD@@

## Summary
INC-2291 began affecting customers at 14:07 UTC on 2026-07-15 and ended at 15:49 UTC the same day. All times in this document are UTC.

## Root cause
The intermediate TLS certificate on the token-vault gateway expired at 14:05 UTC. Calls from the payments API to the vault were rejected, so card tokenisation failed. The database failover seen at 14:20 UTC was a symptom of retry storms, not the cause.

## Impact
Failed transactions: EU region 18,420; US region 7,315.

## Classification
Final severity: SEV-1 (initially declared SEV-2 during the incident; re-classified under severity policy v3).
@@PAD@@
"""),
 dict(name="postmortem-draft-v2.md", title="Post-mortem draft v2 (SUPERSEDED)", core="""Status: DRAFT v2, 2026-07-21. SUPERSEDED by v3.

@@PAD@@

## Summary
Customer impact: 14:07 UTC to 15:31 UTC (the 15:31 time was when the first mitigation was applied; full recovery was later). Root cause: expired certificate under investigation; database failover still listed as contributing factor. Failed transactions: EU 18,420; US 7,100 (later corrected).
@@PAD@@
"""),
 dict(name="slack-export-incident-channel.md", title="Chat export: #inc-2291 (verbatim, unreviewed)", core="""Raw export of the incident channel. Timestamps are shown in the exporter's local time (CEST, UTC+2). This is a chat transcript, not an authoritative record.

@@PAD@@

16:11 sam: payments are failing, looks like the DB primary flipped
16:19 priya: confirmed failover at 16:20 on payments-db, probably the root cause
16:34 sam: declaring sev-2
17:49 dana: recovered, closing the bridge
@@PAD@@
"""),
 dict(name="pager-log-export.md", title="Pager log export", core="""Export from the paging system, in CEST (UTC+2) as displayed in the vendor console.

@@PAD@@

16:07 first customer-facing error alert fired.
16:12 on-call acknowledged.
17:49 incident marked resolved.
@@PAD@@
"""),
 dict(name="timeline-final.md", title="Incident timeline (final, UTC)", core="""Authoritative timeline for INC-2291, UTC, compiled by the incident commander and approved with the final post-mortem.

@@PAD@@

| Time (UTC) | Event |
|---|---|
| 14:05 | Intermediate certificate on token-vault gateway expires |
| 14:07 | Customer impact begins (first failed tokenisation) |
| 14:12 | On-call acknowledges alert |
| 14:20 | Database failover triggered by retry storm |
| 14:34 | Severity declared as SEV-2 |
| 15:02 | New certificate issued |
| 15:31 | First mitigation deployed (partial recovery in EU) |
| 15:49 | Full recovery confirmed; impact ends |
@@PAD@@
"""),
 dict(name="severity-policy-v2.md", title="Severity policy v2 (SUPERSEDED)", core="""Severity policy v2, effective 2025-02-01. Superseded by v3 effective 2026-06-01.

@@PAD@@

SEV-1: a payment outage affecting more than 25% of merchants for over 120 minutes. SEV-2: an outage affecting more than 10% of merchants for over 30 minutes. This incident (102 minutes, EU and US) would have been SEV-2 under this policy.
@@PAD@@
"""),
 dict(name="severity-policy-v3.md", title="Severity policy v3 (in force from 2026-06-01)", core="""Severity policy v3, effective 2026-06-01, replacing v2.

@@PAD@@

SEV-1: any payment-processing outage that lasts more than 60 minutes and affects more than 10% of active merchants, or any outage affecting card tokenisation in more than one region. SEV-2: an outage under those thresholds that lasts more than 30 minutes. Re-classification to a higher severity after the fact is required when the final timeline meets the SEV-1 definition.
@@PAD@@
"""),
 dict(name="sla-credit-policy-v1.md", title="SLA credit policy v1 (SUPERSEDED)", core="""SLA credit policy v1, effective 2024-04-01, superseded by v2 effective 2026-01-01.

@@PAD@@

Credit for a payment outage exceeding 90 minutes: 10% of the affected customer's monthly platform fee.
@@PAD@@
"""),
 dict(name="sla-credit-policy-v2.md", title="SLA credit policy v2 (in force from 2026-01-01)", core="""SLA credit policy v2, effective 2026-01-01, replacing v1.

@@PAD@@

Credit for a payment outage exceeding 90 minutes: 15% of the affected customer's monthly platform fee. Credits are computed on the sum of the monthly platform fees of all affected customers. Outages of 90 minutes or less earn 5%.
@@PAD@@
"""),
 dict(name="customer-impact-report.md", title="Customer impact report", core="""Finance and customer-success report on INC-2291, 2026-07-22.

@@PAD@@

Affected customers (those with at least one failed transaction): 61 enterprise accounts with a combined monthly platform fee of EUR 1,240,000. A further 140 self-serve accounts were affected but pay no platform fee and receive no credit.
@@PAD@@
"""),
 dict(name="action-items-register-v1.md", title="Action item register v1 (SUPERSEDED)", core="""Action items from the post-mortem review, register v1, 2026-07-29. Superseded by v2.

@@PAD@@

AP-1 Rotate all gateway certificates: owner Hugo Eriksen, due 2026-08-07.
AP-4 Rotate token-vault certificate and add expiry monitoring: owner Priya Raman, due 2026-08-07.
@@PAD@@
"""),
 dict(name="action-items-register-v2.md", title="Action item register v2 (latest)", core="""Action items register v2, 2026-08-03. Replaces v1; supplies the current due dates.

@@PAD@@

AP-1 Rotate all gateway certificates: owner Hugo Eriksen, due 2026-08-07 (unchanged).
AP-4 Rotate token-vault certificate and add expiry monitoring: owner Priya Raman, due 2026-08-14 (moved by one week after the dependency on the key-ceremony slot was identified).
AP-7 Retire the old alert channel: owner Dana Okafor, due 2026-09-04.
@@PAD@@
"""),
 dict(name="alerting-review.md", title="Alerting review for INC-2291", core="""Review of why the warning was not acted on, by the monitoring owners.

@@PAD@@

The certificate-expiry rule fired a warning at 7 days before expiry (2026-07-08). It was routed to the Slack channel #platform-alerts-old, which was archived from on-call rotations in 2026-03 but not removed from the alert routing table. The newer channel #platform-alerts-live was never subscribed to the rule. Nobody was paged.
@@PAD@@
"""),
 dict(name="other-incident-INC-2204.md", title="Post-mortem summary: INC-2204 (unrelated, 2026-03)", core="""Summary of an earlier, unrelated incident.

@@PAD@@

INC-2204 was a search-latency incident on 2026-03-11. Impact began at 09:12 UTC and lasted 38 minutes. Root cause: a misconfigured cache TTL. Severity SEV-2. Alert channel used: #search-alerts. Failed requests: 6,030.
@@PAD@@
"""),
]
FILLER = [
 ("oncall-handbook-excerpt.md", "On-call handbook excerpt"),
 ("reliability-council-notes-2026-06.md", "Reliability council notes, June 2026"),
 ("reliability-council-notes-2026-07.md", "Reliability council notes, July 2026"),
 ("reliability-council-notes-2026-08.md", "Reliability council notes, August 2026"),
 ("postmortem-template.md", "Post-mortem template and guidance"),
 ("change-advisory-board-notes.md", "Change advisory board notes"),
 ("capacity-planning-notes.md", "Capacity planning notes"),
 ("support-playbook.md", "Customer support playbook"),
 ("security-guild-notes.md", "Security guild notes"),
 ("faq-incident-management.md", "Incident management FAQ"),
 ("runbook-token-vault.md", "Token vault runbook (generic operations)"),
 ("quarterly-reliability-review.md", "Quarterly reliability review"),
]
EXTRA_TOPICS = ["pager rotations", "certificate inventory", "tokenisation latency", "retry budgets", "status page wording"]

PARTS = [
 dict(id="F1", kind="fact", desc="impact began 14:07 UTC (pager/chat in CEST say 16:07)",
      all=[r"14:07"]),
 dict(id="F2", kind="fact", desc="14:07->15:49 = 102 minutes (draft v1 15:31, v2 84 min)",
      all=[r"\b102\b|1\s*h(ours?|r)?\s*(and\s*)?42|1:42"]),
 dict(id="F3", kind="fact", desc="expired certificate, not DB failover",
      all=[r"expired.{0,60}certificate|certificate.{0,60}expir"]),
 dict(id="F4", kind="fact", desc="18,420 + 7,315 = 25,735",
      all=[r"25[,.\s']?735"]),
 dict(id="F5", kind="fact", desc="15% (policy v2) of 1,240,000 = 186,000 (v1 10% = 124,000)",
      all=[r"186[,.\s']?000"]),
 dict(id="F6", kind="fact", desc="AP-4 due 2026-08-14 (register v2, not v1 08-07)",
      all=[r"2026-08-14|14(th)?\s+Aug(ust)?\.?,?\s+2026|Aug(ust)?\.?\s+14(th)?,?\s+2026|14[./]08[./]2026"]),
 dict(id="F7", kind="fact", desc="#platform-alerts-old",
      all=[r"platform-alerts-old"]),
 dict(id="F8", kind="fact", desc="SEV-1 final (policy v3), not SEV-2",
      all=[r"SEV[- ]?1\b"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# INC-2291 brief

1. **Impact start.** 14:07 UTC on 2026-07-15 [timeline-final.md] [postmortem-final-v3.md]. The 16:07 in the pager log is CEST (UTC+2). [pager-log-export.md]
2. **Duration.** 14:07 to 15:49 UTC = 102 minutes. [timeline-final.md] [postmortem-final-v3.md] The 15:31 time was only the first partial mitigation.
3. **Root cause.** The intermediate TLS certificate on the token-vault gateway expired at 14:05 UTC; the DB failover was a symptom, not the cause. [postmortem-final-v3.md]
4. **Failed transactions.** 18,420 (EU) + 7,315 (US) = 25,735. [postmortem-final-v3.md]
5. **SLA credit.** Policy v2 (in force from 2026-01-01) gives 15% for outages over 90 minutes [sla-credit-policy-v2.md]; 15% of EUR 1,240,000 monthly fees [customer-impact-report.md] = EUR 186,000.
6. **AP-4 due date.** 2026-08-14 per register v2. [action-items-register-v2.md]
7. **Alert channel.** #platform-alerts-old. [alerting-review.md]
8. **Severity.** SEV-1 under severity policy v3 [severity-policy-v3.md] (declared SEV-2 during the incident). [postmortem-final-v3.md]
"""

BAD_BRIEF = """1. 16:07. 2. 84 minutes. 3. database failover. 4. 18,420. 5. EUR 124,000. 6. 2026-08-07. 7. #search-alerts. 8. SEV-2. [postmortem-draft-v1.md] [slack-export-incident-channel.md] [pager-log-export.md] [action-items-register-v1.md] [sla-credit-policy-v1.md]"""
