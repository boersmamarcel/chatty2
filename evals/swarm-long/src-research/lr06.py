"""lr06: clinical protocol amendment (CARDIO-ST3 trial)."""

TITLE = "Clinical protocol amendment: CARDIO-ST3 trial"
INTRO = ("CARDIO-ST3 is a multi-site phase 3 trial. Amendment 3 has just been through ethics review. The study "
         "manager needs a brief establishing what governs at the sites today, from the protocol versions and "
         "amendments, committee letters, site memos, exports and manuals in docs/.")
QUESTIONS = [
    "On what date did the current protocol amendment (Amendment 3) take effect at the sites?",
    "How many participants must be randomised under Amendment 3 (evaluable target plus the stated dropout allowance, rounded up to a whole participant)?",
    "At what week is the primary endpoint assessed under the current protocol?",
    "What is the maximum daily dose, in mg, for a participant with moderate renal impairment (eGFR 30 to below 45) under the current protocol?",
    "A participant at site 014 was randomised on 2026-04-14. On what date does the window for Visit 5 open?",
    "What are the current reporting deadlines for a serious adverse event: to the sponsor (hours) and, for a fatal or life-threatening unexpected event, to the regulatory authority (calendar days)?",
    "Who is the principal investigator at site 014 today?",
    "What eGFR threshold excludes a participant from enrolment under the current protocol?",
]

DOCS = [
 dict(name="protocol-v1-original.md", title="Protocol v1.0 (original, SUPERSEDED)", core="""CARDIO-ST3 protocol v1.0, 2024-11-12. Fully superseded by later amendments where they address the same topic.

@@PAD@@

Planned sample size: 480 evaluable participants. Primary endpoint assessed at week 12. Maximum daily dose 60 mg; with renal impairment (eGFR 30 to below 45) maximum 30 mg. Exclusion: eGFR below 45. SAE to sponsor within 72 hours; fatal or life-threatening unexpected events to the authority within 7 calendar days. Visit 5 at Day 56, window plus or minus 7 days.
@@PAD@@
"""),
 dict(name="amendment-2-summary.md", title="Amendment 2 (protocol v3.0, SUPERSEDED by Amendment 3)", core="""Amendment 2, protocol v3.0, signed by the sponsor on 2025-08-01, ethics approval and site effective date 2025-09-10. Superseded by Amendment 3 on the topics it changes.

@@PAD@@

Changes: sample size raised to 540 evaluable participants; maximum daily dose raised to 80 mg, renal impairment (eGFR 30 to below 45) maximum 60 mg; SAE to sponsor within 48 hours. Primary endpoint timepoint unchanged at week 12. Exclusion threshold unchanged (eGFR below 45).
@@PAD@@
"""),
 dict(name="amendment-3-protocol-v4.md", title="Amendment 3 (protocol v4.0, CURRENT)", core="""Amendment 3, protocol v4.0, signed by the sponsor on 2026-02-17. An amendment takes effect at a site on the date of ethics committee (IRB) approval, not on the sponsor signature date. Topics not mentioned here keep their Amendment 2 wording.

@@PAD@@

## Section 4 Sample size
4.1 The target is 540 evaluable participants. Because of the longer follow-up, a dropout allowance of 12% of the evaluable target is added to determine the number to randomise (round up to a whole participant).

## Section 5 Eligibility
5.3 Exclusion: eGFR below 30 mL/min/1.73 m2 at screening (previously below 45).

## Section 6 Dosing
6.1 The maximum daily dose is 80 mg. 6.2 For moderate renal impairment (eGFR 30 to below 45) the maximum daily dose is reduced by 50%.

## Section 7 Schedule
7.1 The primary endpoint is assessed at week 24 (previously week 12). 7.2 Day 0 is the date of randomisation. Visit 5 takes place at Day 84, with a window of plus or minus 7 days.

## Section 9 Safety reporting
9.1 Investigators report any SAE to the sponsor within 24 hours of becoming aware. 9.2 The sponsor reports fatal or life-threatening unexpected events to the regulatory authority within 5 calendar days.
@@PAD@@
"""),
 dict(name="irb-approval-letter-amendment-3.md", title="IRB approval letter for Amendment 3", core="""Letter from the central Institutional Review Board, dated 2026-03-09, confirming approval of Amendment 3.

@@PAD@@

The IRB approved Amendment 3 (protocol v4.0) at its meeting of 5 March 2026; the approval date is 2026-03-05 and the letter is issued four days later. Sites may implement Amendment 3 from the approval date. The approved protocol is the version signed by the sponsor on 2026-02-17.
@@PAD@@
"""),
 dict(name="irb-approval-letter-amendment-2.md", title="IRB approval letter for Amendment 2", core="""Letter from the central IRB, dated 2025-09-12.

@@PAD@@

Approval of Amendment 2 (protocol v3.0) was granted on 2025-09-10. This letter is superseded in effect by the approval of Amendment 3.
@@PAD@@
"""),
 dict(name="sponsor-memo-sample-size.md", title="Sponsor memo on sample size", core="""Sponsor biostatistics memo, 2026-02-10, informing Amendment 3.

@@PAD@@

The 480 evaluable participants of protocol v1.0 were never enough for the revised effect size. Amendment 2 raised it to 540 evaluable. Amendment 3 keeps 540 evaluable and adds a dropout allowance as set out in protocol section 4. The memo itself carries no operative numbers for the randomised total.
@@PAD@@
"""),
 dict(name="site-014-contact-memo.md", title="Site 014 contact memo (authoritative for site personnel)", core="""Site management memo, 2026-01-20. Per the study operations manual, site contact memos override the CTMS export for named personnel.

@@PAD@@

Dr. Emeka Adeyemi became principal investigator at site 014 on 2026-01-15, replacing Dr. Hannah Brandt, who has moved to site 022 as sub-investigator. The CTMS record will be corrected at the next quarterly update.
@@PAD@@
"""),
 dict(name="ctms-export-2025-12.md", title="CTMS export, December 2025 (stale)", core="""Export from the clinical trial management system, 2025-12-31. Stale for personnel changes made later.

@@PAD@@

Site 014: principal investigator Dr. Hannah Brandt; sub-investigator Dr. Lukas Veen. Site 022: principal investigator Dr. Marta Quist.
@@PAD@@
"""),
 dict(name="study-operations-manual.md", title="Study operations manual (excerpt on document precedence)", core="""Study operations manual excerpt, version 5.

@@PAD@@

Document precedence: (1) the approved protocol at its latest approved amendment, (2) IRB letters, (3) site contact memos for named site personnel, which override system exports, (4) the investigator brochure, (5) CTMS exports and training slides. Training slides are never authoritative.
@@PAD@@
"""),
 dict(name="investigator-training-slides-2025.md", title="Investigator training slides (2025, outdated)", core="""Slides from the investigator meeting of 2025-10-08 (Amendment 2 era). Not authoritative.

@@PAD@@

Slide 14: SAE to sponsor within 48 hours. Slide 15: primary endpoint at week 12. Slide 16: eGFR exclusion below 45; renal dose maximum 60 mg. Slide 17: Visit 5 at Day 56.
@@PAD@@
"""),
 dict(name="investigator-brochure-v7.md", title="Investigator brochure v7 (excerpt)", core="""Investigator brochure v7, 2026-01-12. Background reference; the protocol governs study conduct.

@@PAD@@

Single doses above 80 mg have not been studied. In patients with severe renal impairment (eGFR below 30) exposure roughly doubles and use is not recommended.
@@PAD@@
"""),
 dict(name="visit-schedule-appendix.md", title="Visit schedule appendix (v4.0)", core="""Appendix to protocol v4.0.

@@PAD@@

Visits: V1 screening; V2 randomisation (Day 0); V3 Day 14; V4 Day 42; V5 Day 84 (plus or minus 7 days, so the window opens on Day 77 and closes on Day 91); V6 week 24 primary endpoint visit. Calendar-day counting: the date of V2 is Day 0 and the next calendar day is Day 1.
@@PAD@@
"""),
 dict(name="other-study-note-cardio-st2.md", title="Note on the earlier trial CARDIO-ST2", core="""Summary note on an earlier, separate trial for context. Not applicable to ST3.

@@PAD@@

CARDIO-ST2 randomised 410 participants, assessed its primary endpoint at week 16, excluded participants with eGFR below 60, required SAE reports within 12 hours and had its protocol amendment approved on 2023-11-20.
@@PAD@@
"""),
]
FILLER = [
 ("steering-committee-2026-01.md", "Trial steering committee notes, January 2026"),
 ("steering-committee-2026-03.md", "Trial steering committee notes, March 2026"),
 ("steering-committee-2026-05.md", "Trial steering committee notes, May 2026"),
 ("monitoring-plan.md", "Clinical monitoring plan (generic)"),
 ("data-management-guide.md", "Data management guide"),
 ("pharmacy-manual-excerpt.md", "Pharmacy manual excerpt"),
 ("lab-manual-excerpt.md", "Central laboratory manual excerpt"),
 ("site-feedback-notes.md", "Site feedback notes"),
 ("faq-trial-office.md", "Trial office FAQ"),
 ("recruitment-strategy.md", "Recruitment strategy notes"),
 ("regulatory-submissions-log.md", "Regulatory submissions log (administrative)"),
]
EXTRA_TOPICS = ["screening logs", "visit windows", "drug accountability", "informed consent forms", "monitoring visits"]

PARTS = [
 dict(id="F1", kind="fact", desc="effective at IRB approval 2026-03-05 (not sponsor 02-17 / letter 03-09)",
      all=[r"2026-03-05|5(th)?\s+March\s+2026|March\s+5(th)?,?\s+2026|05[./]03[./]2026"]),
 dict(id="F2", kind="fact", desc="540 x 1.12 = 604.8 -> 605",
      all=[r"\b605\b"]),
 dict(id="F3", kind="fact", desc="week 24 (not 12)",
      all=[r"\b(week|wk)\s*24\b|\b24[\s-]*weeks?\b"]),
 dict(id="F4", kind="fact", desc="80 mg halved = 40 mg (not 60/30)",
      all=[r"\b40\s*mg\b"]),
 dict(id="F5", kind="fact", desc="Day 0 = randomisation; Day 77 = 2026-06-30",
      all=[r"2026-06-30|30(th)?\s+June\s+2026|June\s+30(th)?,?\s+2026|30[./]06[./]2026"]),
 dict(id="F6", kind="fact", desc="SAE 24 hours to sponsor; 5 calendar days to authority",
      all=[r"\b24[\s-]*hours?\b", r"\b(5|five)\s*(\(5\)\s*)?(calendar[\s-]*)?days?\b"]),
 dict(id="F7", kind="fact", desc="Dr. Emeka Adeyemi (memo overrides CTMS export)",
      all=[r"Adeyemi"]),
 dict(id="F8", kind="fact", desc="exclusion eGFR below 30 (previously 45)",
      all=[r"eGFR", r"(<|below|under|less than|lower than)\s*30\b"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# CARDIO-ST3 amendment 3 brief

1. **Effective date.** Amendments take effect at sites on the IRB approval date [amendment-3-protocol-v4.md]; the IRB approved Amendment 3 on 5 March 2026 (2026-03-05) [irb-approval-letter-amendment-3.md]. The sponsor signature (2026-02-17) and the letter date are not the effective date.
2. **Randomisation target.** 540 evaluable plus 12% dropout allowance = 604.8, rounded up to 605 participants. [amendment-3-protocol-v4.md]
3. **Primary endpoint.** Week 24 (previously week 12). [amendment-3-protocol-v4.md] [visit-schedule-appendix.md]
4. **Renal dose.** Maximum daily dose 80 mg reduced by 50% = 40 mg. [amendment-3-protocol-v4.md]
5. **Visit 5 window.** Day 0 is the randomisation date; Visit 5 is at Day 84 +/- 7 days, so the window opens on Day 77: 2026-04-14 + 77 days = 2026-06-30. [visit-schedule-appendix.md] [amendment-3-protocol-v4.md]
6. **SAE reporting.** To the sponsor within 24 hours; fatal or life-threatening unexpected events to the authority within 5 calendar days. [amendment-3-protocol-v4.md] (Older 48/72 hours and 7 days are superseded. [amendment-2-summary.md] [protocol-v1-original.md])
7. **Site 014 PI.** Dr. Emeka Adeyemi, since 2026-01-15; the site memo overrides the stale CTMS export. [site-014-contact-memo.md] [study-operations-manual.md] [ctms-export-2025-12.md]
8. **Exclusion.** eGFR below 30 mL/min/1.73 m2 at screening (previously below 45). [amendment-3-protocol-v4.md]
"""

BAD_BRIEF = """1. 2026-02-17. 2. 540. 3. week 12. 4. 60 mg. 5. 2026-06-02. 6. 48 hours and 7 days. 7. Dr. Hannah Brandt. 8. eGFR below 45. [investigator-training-slides-2025.md] [amendment-2-summary.md] [ctms-export-2025-12.md] [protocol-v1-original.md] [sponsor-memo-sample-size.md]"""
