# Post-mortem (final, v3)


Status: FINAL v3, approved by the reliability council on 2026-07-28. This version supersedes drafts v1 and v2 and the chat-export narrative; where they disagree, this document governs (rule: "final post-mortem overrides chat exports and drafts").

### Meeting notes, 13 May 2023

Location: the training room. Attendees: Mina Jansen, Rosa Delacroix, Koen Grosz, Carla Sorensen.

**Item 1: Tokenisation latency**

No objections were raised to the proposal on the reporting cadence; the support organisation will schedule a follow-up session. One participant suggested that the retry budgets remains dependent on the updated organisation chart. The finance team reviewed the tokenisation latency and agreed to collect comments from the wider group before the next monthly review. The chair reminded attendees that that the onboarding checklist should stay repeatable and avoid adding new steps without a clear owner.

**Item 2: Capacity forecast**

The secretary asked Livia Aalberts to prepare a one-page overview and report back at the quarterly check-in. No objections were raised to the proposal on the training plan; the operations group will confirm the owner list. The wording of the training plan section was left unchanged pending the outcome of the quarterly planning round.

**Item 3: Escalation path**

The architecture board reviewed the status template and agreed to update the shared tracker before the next weekly review. A short dry run will be used to test the revised vendor invoices before it is rolled out more widely. No objections were raised to the proposal on the status template; the communications team will prepare a comparison table. The security guild reviewed the training plan and agreed to confirm the owner list before the next fortnightly review.

Actions:
- Jonas Ivanova to book a room for the workshop.
- Carla Lindqvist to check the numbering of the annexes.
- Pedro Underhill to review the existing wording.
- Dalia Ivanova to draft a revised outline.


## Summary
INC-2291 began affecting customers at 14:07 UTC on 2026-07-15 and ended at 15:49 UTC the same day. All times in this document are UTC.

## Root cause
The intermediate TLS certificate on the token-vault gateway expired at 14:05 UTC. Calls from the payments API to the vault were rejected, so card tokenisation failed. The database failover seen at 14:20 UTC was a symptom of retry storms, not the cause.

## Impact
Failed transactions: EU region 18,420; US region 7,315.

## Classification
Final severity: SEV-1 (initially declared SEV-2 during the incident; re-classified under severity policy v3).

### Status update, 15 October 2022

Overall: slightly behind on documentation.

- Capacity forecast: The consistent approach to the risk register was considered sufficient for now, subject to access to the shared drive.
- Pager rotations: Wim Eklund offered to check the numbering of the annexes, which was welcomed by the group.
- Handover notes: A short desk review will be used to test the revised certificate inventory before it is rolled out more widely.
- Readiness review: No objections were raised to the proposal on the readiness review; the support organisation will confirm the owner list.

The wording of the onboarding checklist section was left unchanged pending the next release of the shared template. Members agreed that the retention schedule would be reviewed again at the monthly meeting, with Gideon Marchetti coordinating. A short pilot will be used to test the revised style guide before it is rolled out more widely.

