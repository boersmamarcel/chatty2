"""lr05: grant application (OceanSense consortium, Blue Horizon call)."""

TITLE = "Grant application: OceanSense consortium, Blue Horizon Call 2026"
INTRO = ("The OceanSense consortium is preparing an application to the Blue Horizon Marine Technology Call 2026. "
         "The coordinator needs a brief fixing deadlines, funding amounts, eligibility and administrative facts "
         "from the call documents (several versions), consortium papers and letters in docs/.")
QUESTIONS = [
    "What is the submission deadline (date) for proposals under the call as most recently amended?",
    "What is the maximum grant in EUR we can request: total eligible costs after the exclusions, times the funding rate that applies to our coordinator?",
    "How many EUR of indirect costs may be claimed on our direct personnel costs?",
    "What is the page limit for the narrative proposal?",
    "On what date does the project end (last day), given the planned start date and duration?",
    "Which consortium partner is ineligible under the current eligibility annex?",
    "How many EUR of cash co-funding do the partner commitment letters add up to (in-kind contributions excluded)?",
    "What is the ethics approval reference number covering this project?",
]

DOCS = [
 dict(name="call-text-v1.md", title="Blue Horizon Call 2026: call text v1 (SUPERSEDED)", core="""Call text, version 1, published 2026-05-04. SUPERSEDED by version 2 (2026-07-01).

@@PAD@@

Deadline: 30 October 2026, 17:00 CET.
Funding rates: universities and research organisations 70%; SMEs 50%; large enterprises 40% of eligible costs.
Indirect costs: flat rate of 20% of direct personnel costs.
Narrative proposal: maximum 25 pages.
@@PAD@@
"""),
 dict(name="call-text-v2.md", title="Blue Horizon Call 2026: call text v2 (current)", core="""Call text, version 2, published 2026-07-01. Replaces version 1 in full. Where an FAQ or guidance note disagrees with this text, this text prevails.

@@PAD@@

## 3 Deadline
Proposals must be submitted by 6 November 2026, 17:00 CET. The portal closes automatically.

## 5 Funding rates
Universities and research organisations: 70% of eligible costs. SMEs: 60% of eligible costs. Large enterprises: 40% of eligible costs. The rate is determined by the status of the coordinating applicant.

## 6 Eligible costs
Eligible costs are the budgeted costs of the project excluding the cost categories listed in Annex C.

## 7 Indirect costs
A flat rate of 25% of direct personnel costs covers all indirect costs. No other indirect costs may be claimed.

## 9 Proposal format
The narrative proposal (Part B) is limited to 30 pages, excluding annexes and letters.
@@PAD@@
"""),
 dict(name="call-annex-c-ineligible-costs.md", title="Call Annex C: ineligible cost categories (v2)", core="""Annex C to call text v2.

@@PAD@@

The following are not eligible: (1) depreciation of existing equipment; (2) bank charges and interest; (3) currency exchange losses; (4) fines. Equipment purchased specifically for the project is eligible. Applicants must therefore deduct any budgeted depreciation of existing equipment from their total budgeted costs when computing eligible costs.
@@PAD@@
"""),
 dict(name="call-eligibility-annex-v1.md", title="Call eligibility annex v1 (SUPERSEDED)", core="""Eligibility annex v1, 2026-05-04. Superseded by v2.

@@PAD@@

Eligible countries for beneficiaries: Norway, Netherlands, Denmark, Iceland, Zavria, Ireland, Portugal.
@@PAD@@
"""),
 dict(name="call-eligibility-annex-v2.md", title="Call eligibility annex v2 (current)", core="""Eligibility annex v2, 2026-07-01. Replaces v1.

@@PAD@@

Eligible countries for beneficiaries: Norway, Netherlands, Denmark, Iceland, Ireland, Portugal. Zavria was removed following the sanctions review of June 2026; entities registered there cannot be beneficiaries, even if named in an earlier consortium agreement.
@@PAD@@
"""),
 dict(name="applicant-faq-2026-06.md", title="Applicant FAQ (June 2026, outdated)", core="""Applicant FAQ compiled by the call secretariat on 2026-06-15, before call text v2. Guidance only.

@@PAD@@

Q: What is the page limit for Part B? A: 25 pages. Q: What is the deadline? A: 30 October 2026. Q: What is the indirect cost rate? A: 20% of direct personnel costs.
@@PAD@@
"""),
 dict(name="consortium-overview.md", title="Consortium overview", core="""Overview of the OceanSense consortium, version of 2026-08-10.

@@PAD@@

Coordinator: Brevik Systems AS (Norway), an SME. Partners: Nordkap University (Norway), Oceanic Instruments Ltd (registered in Zavria). Planned project start: 1 March 2027. Planned duration: 36 months. A project ends on the last day of its final month.
@@PAD@@
"""),
 dict(name="budget-sheet-v3.md", title="Budget sheet v3", core="""Consolidated budget sheet, version 3, 2026-09-02 (replaces v1 and v2).

@@PAD@@

| Line | EUR |
|---|---|
| Total budgeted project costs | 2,400,000 |
| of which direct personnel costs | 640,000 |
| of which depreciation of existing equipment | 180,000 |
| of which project-specific equipment | 310,000 |
| of which travel and dissemination | 95,000 |
@@PAD@@
"""),
 dict(name="budget-sheet-v1.md", title="Budget sheet v1 (SUPERSEDED)", core="""Budget sheet v1, 2026-07-15, superseded by v3.

@@PAD@@

Total budgeted project costs: EUR 2,150,000. Direct personnel costs: EUR 590,000. Depreciation of existing equipment: EUR 120,000.
@@PAD@@
"""),
 dict(name="letter-nordkap-university.md", title="Commitment letter: Nordkap University", core="""Letter of commitment, 2026-09-04.

@@PAD@@

Nordkap University commits a cash contribution of EUR 150,000 to the project. It makes no in-kind commitment.
@@PAD@@
"""),
 dict(name="letter-brevik-systems.md", title="Commitment letter: Brevik Systems AS", core="""Letter of commitment, 2026-09-05.

@@PAD@@

Brevik Systems AS commits a cash contribution of EUR 90,000 and an in-kind contribution (staff time and test-bench access) valued at EUR 40,000.
@@PAD@@
"""),
 dict(name="letter-oceanic-instruments.md", title="Commitment letter: Oceanic Instruments Ltd", core="""Letter of commitment, 2026-09-06.

@@PAD@@

Oceanic Instruments Ltd commits an in-kind contribution of sensor prototypes valued at EUR 60,000. It makes no cash commitment.
@@PAD@@
"""),
 dict(name="ethics-approval-letter.md", title="Ethics review board approval letter", core="""Letter from the Ethics Review Board, 2026-08-21, covering the OceanSense project (sea-mammal acoustic monitoring).

@@PAD@@

The board approves the protocol under reference ERB-2026-0417. This replaces the provisional reference ERB-2026-0399 issued on 2026-06-12, which lapsed when the protocol was revised.
@@PAD@@
"""),
 dict(name="ethics-previous-project.md", title="Ethics record: previous project SEAWATCH", core="""Ethics record of an earlier, unrelated project (SEAWATCH, completed 2025).

@@PAD@@

SEAWATCH ethics reference: ERB-2025-0417. Do not reuse for new applications.
@@PAD@@
"""),
 dict(name="competing-consortium-intel.md", title="Competitor intelligence note", core="""Note on another likely applicant, the Kelpwave consortium.

@@PAD@@

Kelpwave is led by a large enterprise (40% rate), plans a 48-month project starting 1 January 2027, and requests a grant of EUR 1,900,000. Their ethics reference is ERB-2026-0102.
@@PAD@@
"""),
]
FILLER = [
 ("consortium-meeting-2026-07.md", "Consortium meeting notes, July 2026"),
 ("consortium-meeting-2026-08.md", "Consortium meeting notes, August 2026"),
 ("consortium-meeting-2026-09.md", "Consortium meeting notes, September 2026"),
 ("work-package-descriptions.md", "Work package descriptions (draft)"),
 ("dissemination-plan.md", "Dissemination and outreach plan"),
 ("data-management-plan.md", "Data management plan (draft)"),
 ("risk-assessment.md", "Project risk assessment"),
 ("grants-office-guidance.md", "Grants office guidance (generic)"),
 ("reviewer-criteria-notes.md", "Notes on reviewer criteria"),
 ("faq-consortium-admin.md", "Consortium administration FAQ"),
 ("sensor-deployment-notes.md", "Sensor deployment planning notes"),
 ("gender-equality-plan.md", "Gender equality and inclusion plan"),
]
EXTRA_TOPICS = ["sensor buoys", "hydrophone arrays", "work packages", "deliverables", "reviewer feedback"]

PARTS = [
 dict(id="F1", kind="fact", desc="deadline 6 Nov 2026 (v2), not 30 Oct",
      all=[r"2026-11-06|6(th)?\s+Nov(ember)?\.?,?\s+2026|Nov(ember)?\.?\s+6(th)?,?\s+2026|06[./]11[./]2026"]),
 dict(id="F2", kind="fact", desc="(2,400,000-180,000) x 60% SME = 1,332,000",
      all=[r"1[.,]332\s*(m\b|mn|million)|1[,.\s']?332[,.\s']?000"]),
 dict(id="F3", kind="fact", desc="25% x 640,000 = 160,000 (v1 20% = 128,000)",
      all=[r"160[,.\s']?000|160\s*k\b"]),
 dict(id="F4", kind="fact", desc="30 pages (call v2 beats FAQ 25)",
      all=[r"\b(30|thirty)\b[\s-]*pages?|page\s*limit\s*(is|of|:)?\s*(a\s*)?(maximum\s*(of\s*)?)?\b30\b"]),
 dict(id="F5", kind="fact", desc="1 Mar 2027 + 36 months -> 28 Feb 2030",
      all=[r"2030-02-28|28(th)?\s+Feb(ruary)?\.?,?\s+2030|Feb(ruary)?\.?\s+28(th)?,?\s+2030|28[./]02[./]2030"]),
 dict(id="F6", kind="fact", desc="Oceanic Instruments (Zavria removed in annex v2)",
      all=[r"Oceanic"]),
 dict(id="F7", kind="fact", desc="150,000 + 90,000 cash = 240,000 (in-kind excluded)",
      all=[r"240[,.\s']?000|240\s*k\b"]),
 dict(id="F8", kind="fact", desc="ERB-2026-0417 (not 2025-0417 / provisional 0399)",
      all=[r"ERB-2026-0417"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# OceanSense application brief

1. **Deadline.** 6 November 2026, 17:00 CET, per call text v2. [call-text-v2.md] (v1's 30 October and the June FAQ are outdated. [call-text-v1.md] [applicant-faq-2026-06.md])
2. **Maximum grant.** Budget EUR 2,400,000 [budget-sheet-v3.md] less EUR 180,000 depreciation of existing equipment (ineligible per Annex C [call-annex-c-ineligible-costs.md]) = EUR 2,220,000 eligible; our coordinator Brevik Systems is an SME [consortium-overview.md], rate 60% [call-text-v2.md]: EUR 1,332,000.
3. **Indirect costs.** 25% of EUR 640,000 direct personnel = EUR 160,000. [call-text-v2.md] [budget-sheet-v3.md]
4. **Page limit.** 30 pages for Part B. [call-text-v2.md]
5. **Project end.** Start 1 March 2027, 36 months, ends 28 February 2030. [consortium-overview.md]
6. **Ineligible partner.** Oceanic Instruments Ltd, registered in Zavria, which eligibility annex v2 removed. [call-eligibility-annex-v2.md] [consortium-overview.md]
7. **Cash co-funding.** EUR 150,000 (Nordkap University) [letter-nordkap-university.md] + EUR 90,000 (Brevik Systems) [letter-brevik-systems.md] = EUR 240,000; in-kind amounts excluded.
8. **Ethics reference.** ERB-2026-0417. [ethics-approval-letter.md]
"""

BAD_BRIEF = """1. 30 October 2026. 2. EUR 1,110,000. 3. EUR 128,000. 4. 25 pages. 5. 1 March 2030. 6. Nordkap University. 7. EUR 280,000. 8. ERB-2025-0417. [call-text-v1.md] [applicant-faq-2026-06.md] [budget-sheet-v1.md] [ethics-previous-project.md] [letter-brevik-systems.md]"""
