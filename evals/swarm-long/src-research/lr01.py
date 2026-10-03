"""lr01: vendor due diligence (Halvorsen Cloud Storage)."""

TITLE = "Vendor due diligence: Halvorsen Cloud Storage"
INTRO = ("Brightmoor Logistics is about to sign up for Halvorsen Cloud Storage. Legal needs a due-diligence "
         "brief that settles the contract facts from the document pile in docs/ (several versions of the "
         "agreement, order forms, security papers, wiki pages and meeting notes).")
QUESTIONS = [
    "Which version of the Master Services Agreement (MSA) currently governs, and on what date did it take effect?",
    "What is the total annual fee in EUR (the base subscription plus any add-ons) under the currently effective order form(s)?",
    "How many days of written notice are required to terminate for convenience? (State the figure that is legally binding.)",
    "On what date does the initial term end (give the last day of the term)?",
    "In which cloud region will Brightmoor's data be hosted?",
    "Which SOC 2 Type II report period is the latest one the vendor has delivered, and on what date did that period end?",
    "What is the liability cap in EUR under the currently effective MSA?",
    "Which subprocessor on the current subprocessor list processes data outside the EEA?",
]

DOCS = [
 dict(name="msa-v1.md", title="Master Services Agreement v1 (SUPERSEDED)", core="""Document status: SUPERSEDED by v2. Version 1, effective 2024-09-02.

@@PAD@@

## 9 Limitation of liability
9.1 Each party's aggregate liability is capped at 0.5 times the fees paid in the preceding twelve months.

## 11 Termination
11.2 Either party may terminate for convenience on 30 days' written notice.

## 14 Availability
14.1 The service targets 99.5% monthly availability.
"""),
 dict(name="msa-v2.md", title="Master Services Agreement v2 (SUPERSEDED)", core="""Document status: SUPERSEDED by v3. Version 2, effective 2025-01-15.

@@PAD@@

## 9 Limitation of liability
9.1 Each party's aggregate liability is capped at 1.0 times the annual fees.

## 11 Termination
11.2 Either party may terminate for convenience on 60 days' written notice.

## 14 Availability
14.1 The service targets 99.9% monthly availability.
"""),
 dict(name="msa-v3.md", title="Master Services Agreement v3", core="""Document status: CURRENT. Version 3, executed 2026-02-03 by both parties. Effective Date: 1 March 2026. This version replaces v2 in full for all order forms placed on or after that date.

@@PAD@@

## 9 Limitation of liability
9.1 Subject to clause 9.3, each party's aggregate liability arising under this Agreement in any contract year is capped at 1.5 times the Annual Fees payable under the order form(s) in force for that year.
9.2 "Annual Fees" means the base subscription fee plus every add-on fee listed in an order form, before taxes.
9.3 The cap does not apply to breach of confidentiality or to wilful misconduct.

@@PAD@@

## 11 Termination
11.2 Either party may terminate for convenience on 90 days' written notice, to take effect no earlier than the end of the first contract year.
11.3 Internal summaries of this clause are for convenience only; the signed text prevails.

## 14 Availability
14.1 The service targets 99.95% monthly availability, with service credits as set out in Schedule B.
"""),
 dict(name="order-form-1.md", title="Order Form 1: Core subscription", core="""Order Form 1 under MSA v3. Customer: Brightmoor Logistics. Signed 2026-02-10.

@@PAD@@

| Item | Detail |
|---|---|
| Effective Date of this order form | 10 February 2026 |
| Commencement Date | 45 days after the Effective Date of this order form |
| Initial Term | 24 months from the Commencement Date; the term expires at 23:59 on the day before the second anniversary of the Commencement Date |
| Base subscription (Archive + Standard tiers) | EUR 184,000 per contract year |

Fees are exclusive of VAT. This order form must be read together with Order Form 2, which carries the add-ons.
@@PAD@@
"""),
 dict(name="order-form-2.md", title="Order Form 2: Premium support add-on", core="""Order Form 2 under MSA v3, signed 2026-02-10 together with Order Form 1. It is co-terminous with Order Form 1.

@@PAD@@

| Item | Detail |
|---|---|
| Premium 24x7 support add-on | EUR 36,500 per contract year |
| Dedicated technical account manager | included in the add-on |

No other add-ons have been ordered. For the avoidance of doubt, the base subscription is stated in Order Form 1 only.
@@PAD@@
"""),
 dict(name="order-form-draft-0.md", title="Order Form draft 0 (never signed)", core="""This draft was circulated on 2026-01-20 and was never signed. It is kept for reference only.

@@PAD@@

Draft base subscription: EUR 171,000 per contract year. Draft premium support: EUR 29,000 per contract year. Draft commencement: on go-live acceptance.
@@PAD@@
"""),
 dict(name="procurement-wiki-vendor-terms.md", title="Procurement wiki: Halvorsen quick facts", core="""Wiki page maintained by the procurement team. Last edited 2026-02-12. Convenience summary only; where it differs from the signed contract, the signed contract wins (see wiki policy page "Sources of truth").

@@PAD@@

- Termination for convenience: 60 days' notice.
- Uptime target: 99.9%.
- Hosting: whatever the security questionnaire says.
@@PAD@@
"""),
 dict(name="dpa-v1.md", title="Data Processing Addendum v1 (SUPERSEDED)", core="""DPA version 1, dated 2025-06-30. SUPERSEDED by DPA v2.

@@PAD@@

## 4 Hosting location
Customer Personal Data is hosted in the AWS region eu-west-1 (Ireland).
@@PAD@@
"""),
 dict(name="dpa-v2.md", title="Data Processing Addendum v2", core="""DPA version 2, signed 2026-02-03 as Schedule C of MSA v3. It replaces DPA v1 in full.

@@PAD@@

## 4 Hosting location
4.1 Customer Personal Data is hosted in the AWS region eu-central-1 (Frankfurt). The vendor may not move it to another region without prior written consent.

## 6 Subprocessors
6.1 The current list of subprocessors is the subprocessor list referred to in Annex 2, which the vendor republishes whenever it changes.
@@PAD@@
"""),
 dict(name="security-questionnaire-2025.md", title="Security questionnaire answers (2025)", core="""Completed by Halvorsen on 2025-05-14 in response to Brightmoor's standard questionnaire. Answers reflect the vendor's position at that date and pre-date the DPA v2.

@@PAD@@

Q12. Where is customer data hosted? Answer: eu-west-1 (Ireland).
Q31. Do you hold a SOC 2 Type II report? Answer: yes, report period 1 October 2023 to 30 September 2024.
@@PAD@@
"""),
 dict(name="soc2-letter-2025.md", title="SOC 2 Type II bridge and report letter", core="""Letter from the vendor's auditor, dated 2025-11-20.

@@PAD@@

The auditor confirms issuance of two SOC 2 Type II reports for Halvorsen Cloud Storage: the first covering the period 1 October 2023 to 30 September 2024, and the most recent covering the period 1 October 2024 to 30 September 2025. Both reports were unqualified. A third report covering the following period is expected in late 2026.
@@PAD@@
"""),
 dict(name="subprocessor-list-v4.md", title="Subprocessor list v4 (current)", core="""Subprocessor list, version 4, published 2026-01-30 (current). Version 3 is withdrawn.

@@PAD@@

| Subprocessor | Service | Location |
|---|---|---|
| Nordlicht Hosting GmbH | Primary infrastructure | Germany |
| Quartzline Networks BV | Content delivery | Netherlands |
| Brightline Analytics Inc. | Usage analytics | United States |
| Sparrow Mail Oy | Transactional email | Finland |
| Lumen Backup SA | Offsite backup | France |
| Verdant Support Ltd | Ticket tooling | Ireland |

All other processing takes place in the EEA.
@@PAD@@
"""),
 dict(name="subprocessor-list-v3.md", title="Subprocessor list v3 (withdrawn)", core="""Subprocessor list, version 3, published 2025-07-01. WITHDRAWN; replaced by v4.

@@PAD@@

| Subprocessor | Service | Location |
|---|---|---|
| Nordlicht Hosting GmbH | Primary infrastructure | Germany |
| Quartzline Networks BV | Content delivery | Netherlands |
| Corvid Metrics LLC | Usage analytics | United States |
| Sparrow Mail Oy | Transactional email | Finland |
"""),
 dict(name="competitor-vendor-skyvault.md", title="Comparison note: SkyVault Ltd (alternative bidder)", core="""Notes on the alternative bidder SkyVault Ltd, not selected.

@@PAD@@

SkyVault quoted a base subscription of EUR 190,000 per year, hosting in eu-north-1 (Stockholm), 120 days' termination notice, and a liability cap of 2.0 times annual fees. Its latest SOC 2 Type II period ended 31 March 2026.
@@PAD@@
"""),
 dict(name="wiki-sources-of-truth.md", title="Procurement wiki: sources of truth", core="""Wiki policy page. The order of precedence for contract facts is: (1) the signed contract documents in their latest effective version, (2) later signed addenda, (3) vendor questionnaires, (4) wiki summaries. A lower-ranked source never overrides a higher one, and a questionnaire answer never overrides an executed addendum.

@@PAD@@
"""),
]

FILLER = [
 ("steering-notes-2026-01.md", "Vendor selection steering notes, January 2026"),
 ("steering-notes-2026-02.md", "Vendor selection steering notes, February 2026"),
 ("steering-notes-2026-03.md", "Vendor selection steering notes, March 2026"),
 ("legal-review-checklist.md", "Legal review checklist (generic)"),
 ("it-onboarding-plan.md", "IT onboarding plan for storage vendors"),
 ("procurement-policy-excerpt.md", "Procurement policy excerpt"),
 ("finance-budget-notes.md", "Finance budget notes for infrastructure"),
 ("vendor-intro-call-notes.md", "Introductory call notes with several vendors"),
 ("faq-vendor-management.md", "Vendor management FAQ"),
 ("archive-migration-runbook.md", "Archive migration runbook (draft)"),
 ("security-guild-notes.md", "Security guild notes"),
 ("quarterly-ops-review.md", "Quarterly operations review"),
]
EXTRA_TOPICS = ["storage tiers", "egress charges", "object lifecycle rules", "service credits", "vendor audit rights"]

PARTS = [
 dict(id="F1", kind="fact", desc="MSA v3 effective 1 March 2026 (not v2 / 2025-01-15)",
      all=[r"v(ersion)?\s*3|third version", r"2026-03-01|1(st)?\s+March\s+2026|March\s+1(st)?,?\s+2026|01[./]03[./]2026"]),
 dict(id="F2", kind="fact", desc="OF1 184,000 + OF2 36,500 = 220,500",
      all=[r"220[,.\s']?500"]),
 dict(id="F3", kind="fact", desc="90 days notice (signed MSA beats wiki 60)",
      all=[r"\b90\b|ninety"]),
 dict(id="F4", kind="fact", desc="Effective 2026-02-10 + 45d = commencement 2026-03-27; +24m, last day 2028-03-26",
      all=[r"2028-03-26|26(th)?\s+March\s+2028|March\s+26(th)?,?\s+2028|26[./]03[./]2028"]),
 dict(id="F5", kind="fact", desc="eu-central-1 (DPA v2 beats questionnaire/DPA v1 eu-west-1)",
      all=[r"eu-central-1|Frankfurt"]),
 dict(id="F6", kind="fact", desc="latest SOC 2 period ended 30 Sep 2025",
      all=[r"2025-09-30|30(th)?\s+Sep(tember)?\.?,?\s+2025|Sep(tember)?\.?\s+30(th)?,?\s+2025|30[./]09[./]2025"]),
 dict(id="F7", kind="fact", desc="cap 1.5 x 220,500 = 330,750",
      all=[r"330[,.\s']?750"]),
 dict(id="F8", kind="fact", desc="Brightline Analytics (v4 list, not Corvid in v3)",
      all=[r"Brightline"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# Due-diligence brief: Halvorsen Cloud Storage

1. **Governing MSA.** MSA v3, effective 1 March 2026 (executed 2026-02-03). [msa-v3.md] It replaces v2 (effective 2025-01-15), which is superseded. [msa-v2.md]
2. **Annual fee.** EUR 184,000 base subscription [order-form-1.md] plus EUR 36,500 premium support [order-form-2.md] = EUR 220,500 per contract year.
3. **Termination notice.** 90 days, per the signed MSA v3 clause 11.2 [msa-v3.md]. The wiki's 60 days is a convenience summary that loses to the signed contract. [procurement-wiki-vendor-terms.md] [wiki-sources-of-truth.md]
4. **End of initial term.** Order Form 1 is effective 10 February 2026; commencement is 45 days later, 27 March 2026; the 24-month term expires on the day before the second anniversary: 26 March 2028. [order-form-1.md]
5. **Hosting region.** eu-central-1 (Frankfurt) per DPA v2. [dpa-v2.md] The 2025 questionnaire's eu-west-1 is outdated. [security-questionnaire-2025.md]
6. **SOC 2.** Latest Type II period is 1 October 2024 to 30 September 2025, ending 30 September 2025. [soc2-letter-2025.md]
7. **Liability cap.** 1.5 times annual fees = 1.5 x 220,500 = EUR 330,750. [msa-v3.md] [order-form-2.md]
8. **Non-EEA subprocessor.** Brightline Analytics Inc. (United States), on list v4. [subprocessor-list-v4.md]
"""

BAD_BRIEF = """1. MSA v2, effective 2025-01-15. 2. EUR 184,000. 3. 60 days. 4. 2028-03-27. 5. eu-west-1. 6. period ended 30 September 2024. 7. EUR 184,000. 8. Corvid Metrics LLC. [msa-v2.md] [msa-v1.md] [dpa-v1.md] [subprocessor-list-v3.md] [order-form-1.md]"""
