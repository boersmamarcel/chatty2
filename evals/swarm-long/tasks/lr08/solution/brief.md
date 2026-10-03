# INC-2291 brief

1. **Impact start.** 14:07 UTC on 2026-07-15 [timeline-final.md] [postmortem-final-v3.md]. The 16:07 in the pager log is CEST (UTC+2). [pager-log-export.md]
2. **Duration.** 14:07 to 15:49 UTC = 102 minutes. [timeline-final.md] [postmortem-final-v3.md] The 15:31 time was only the first partial mitigation.
3. **Root cause.** The intermediate TLS certificate on the token-vault gateway expired at 14:05 UTC; the DB failover was a symptom, not the cause. [postmortem-final-v3.md]
4. **Failed transactions.** 18,420 (EU) + 7,315 (US) = 25,735. [postmortem-final-v3.md]
5. **SLA credit.** Policy v2 (in force from 2026-01-01) gives 15% for outages over 90 minutes [sla-credit-policy-v2.md]; 15% of EUR 1,240,000 monthly fees [customer-impact-report.md] = EUR 186,000.
6. **AP-4 due date.** 2026-08-14 per register v2. [action-items-register-v2.md]
7. **Alert channel.** #platform-alerts-old. [alerting-review.md]
8. **Severity.** SEV-1 under severity policy v3 [severity-policy-v3.md] (declared SEV-2 during the incident). [postmortem-final-v3.md]
