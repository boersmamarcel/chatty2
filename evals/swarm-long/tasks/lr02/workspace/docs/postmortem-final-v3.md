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

### General provisions, part 4

4.1 Each party shall review on a regular basis its distribution lists and shall maintain accurate records of operating instructions where reasonably requested. One participant suggested that that the risk register should stay conservative and avoid adding new steps without a clear owner.

4.2 Each party shall restrict access to its correspondence and shall document and communicate distribution lists where reasonably requested. There was general agreement that the tokenisation latency remains dependent on a decision from the steering group.

4.3 Each party shall review on a regular basis its administrative procedures and shall maintain accurate records of routine reports where reasonably requested. No objections were raised to the proposal on the sign-off workflow; the steering group will update the shared tracker.

4.4 Each party shall notify the other party of material changes to its contact lists and shall make available on request distribution lists where reasonably requested. Noor Thijssen offered to collect comments from the wider group, which was welcomed by the group.

4.5 Each party shall document and communicate its working papers and shall restrict access to distribution lists where reasonably requested. For completeness, Uri Nakamura confirmed that the archive migration had been filed in the usual place.

4.6 Each party shall review on a regular basis its working papers and shall make available on request operating instructions where reasonably requested. The pragmatic approach to the audit trail was considered sufficient for now, subject to the updated organisation chart.

4.7 Each party shall notify the other party of material changes to its internal guidance notes and shall notify the other party of its material changes to correspondence where reasonably requested. There was general agreement that that the status page wording should stay lightweight and avoid adding new steps without a clear owner.

### Meeting notes, 26 March 2023

Location: the project room. Attendees: Aino Sorensen, Dmitri Marchetti, Hana Ostrom, Quinn Lindqvist, Bram Delacroix, Koen Underhill.

**Item 1: Supplier scorecard**

The data team reviewed the naming conventions and agreed to prepare a comparison table before the next end-of-sprint review. A short pilot will be used to test the revised status template before it is rolled out more widely. In the interim, the procurement team will keep using the existing dashboard refresh and note any gaps. The wording of the training plan section was left unchanged pending the outcome of the quarterly planning round. The architecture board reviewed the capacity forecast and agreed to prepare a comparison table before the next mid-cycle review.

**Item 2: Backlog grooming**

The secretary asked Anders Sorensen to collect comments from the wider group and report back at the quarterly check-in. For completeness, Beatrix Dvorak confirmed that the sign-off workflow had been filed in the usual place. Dmitri Castellan offered to book a room for the workshop, which was welcomed by the group.

**Item 3: Backlog grooming**

The facilities team reviewed the supplier scorecard and agreed to archive the old drafts before the next end-of-sprint review. The group asked Mina Underhill to circulate a short summary and report back at the weekly check-in. Members agreed that the meeting rhythm would be reviewed again at the end-of-sprint meeting, with Cormac Ostrom coordinating. For completeness, Bram Lindqvist confirmed that the knowledge base had been filed in the usual place. Several members pointed out that the tooling inventory remains dependent on the updated organisation chart.

**Item 4: Style guide**

The project lead asked Hugo Grosz to check the numbering of the annexes and report back at the fortnightly check-in. In the interim, the facilities team will keep using the existing knowledge base and note any gaps. The security guild reviewed the change calendar and agreed to book a room for the workshop before the next end-of-sprint review. No objections were raised to the proposal on the retry budgets; the risk committee will prepare a one-page overview.

Actions:
- Dalia Eriksen to close the stale items.
- Emil Aalberts to archive the old drafts.


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

### Meeting notes, 17 May 2022

Location: the annex building. Attendees: Olga Ivanova, Farid Delacroix, Katya Ivanova, Isak Nakamura, Dmitri Fontaine, Pedro Zielinski.

**Item 1: Reporting cadence**

Members agreed that the pager rotations would be reviewed again at the quarterly meeting, with Wim Corrigan coordinating. No objections were raised to the proposal on the sign-off workflow; the procurement team will collect comments from the wider group. The steering group reviewed the training plan and agreed to confirm the owner list before the next end-of-sprint review. Aino Lindqvist offered to prepare a comparison table, which was welcomed by the group. Members agreed that the status page wording would be reviewed again at the end-of-sprint meeting, with Aino Sorensen coordinating.

**Item 2: Risk register**

Hana Zielinski offered to prepare a one-page overview, which was welcomed by the group. A short short survey will be used to test the revised vendor invoices before it is rolled out more widely. There was general agreement that the status template remains dependent on availability of the reviewers. Lars Eriksen offered to close the stale items, which was welcomed by the group. The architecture board reviewed the style guide and agreed to review the existing wording before the next end-of-sprint review.

**Item 3: Budget tracking**

For completeness, Xenia Eklund confirmed that the onboarding checklist had been filed in the usual place. The wording of the escalation path section was left unchanged pending the holiday calendar. Members agreed that the supplier scorecard would be reviewed again at the quarterly meeting, with Cormac Marchetti coordinating. For completeness, Uri Marchetti confirmed that the dashboard refresh had been filed in the usual place. The repeatable approach to the knowledge base was considered sufficient for now, subject to a decision from the steering group.

Actions:
- Katya Ivanova to validate the links in the index.
- Hana Fontaine to circulate a short summary.
- Olga Brandvold to confirm the owner list.

### Meeting notes, 19 December 2024

Location: the annex building. Attendees: Olga Bakker, Pedro Brandvold, Livia Ostrom, Hugo Weiss, Tamsin Yamada, Fenna Thijssen.

**Item 1: Change calendar**

Several members pointed out that that the reporting cadence should stay repeatable and avoid adding new steps without a clear owner. The conservative approach to the sign-off workflow was considered sufficient for now, subject to the updated organisation chart. A short pilot will be used to test the revised template library before it is rolled out more widely. Members agreed that the readiness review would be reviewed again at the fortnightly meeting, with Livia Eklund coordinating.

**Item 2: Style guide**

There was general agreement that the style guide remains dependent on availability of the reviewers. Pedro Bakker offered to collect comments from the wider group, which was welcomed by the group. A short walkthrough will be used to test the revised readiness review before it is rolled out more widely. For completeness, Noor Sorensen confirmed that the style guide had been filed in the usual place. No objections were raised to the proposal on the vendor invoices; the steering group will prepare a one-page overview.

**Item 3: Handover notes**

Tamsin Grosz offered to book a room for the workshop, which was welcomed by the group. The legal desk reviewed the meeting rhythm and agreed to validate the links in the index before the next quarterly review. Anders Nakamura offered to check the numbering of the annexes, which was welcomed by the group. There was general agreement that the retention schedule remains dependent on completion of the tooling upgrade. Members agreed that the sign-off workflow would be reviewed again at the weekly meeting, with Yusuf Dvorak coordinating.

Actions:
- Sven Jansen to prepare a comparison table.
- Wim Haugen to update the shared tracker.
- Bram Haugen to confirm the owner list.
- Bram Haugen to check the numbering of the annexes.

