"""lr03: regulatory compliance (Financial Resilience Directive, Marlowe Capital)."""

TITLE = "Regulatory compliance: Financial Resilience Regulation (FRR) at Marlowe Capital"
INTRO = ("Marlowe Capital B.V. must comply with the Financial Resilience Regulation (FRR) on ICT incident reporting "
         "and third-party risk. The compliance officer needs a brief settling what applies to Marlowe today, "
         "from the regulation texts, amendments, internal policies, board minutes and registers in docs/.")
QUESTIONS = [
    "Within how many hours of classifying an incident as major must the initial notification be submitted under the FRR as currently amended?",
    "Marlowe submitted its initial notification for incident MC-77 on 2026-05-08 at 09:30. By what date and time is the intermediate report due?",
    "What is the maximum administrative fine in EUR that can be imposed on Marlowe, based on its most recent audited annual turnover?",
    "Which supervisory authority is Marlowe's competent authority today?",
    "For how many years must Marlowe keep incident records under the stricter of the regulation and the internal policy in force?",
    "How many critical ICT third-party providers are on Marlowe's register after applying the change log?",
    "By what date is Marlowe's next threat-led penetration test due under the FRR as currently amended?",
    "Who is Marlowe's accountable executive for FRR compliance according to the latest board decision?",
]

DOCS = [
 dict(name="frr-2025-14-original.md", title="FRR Regulation 2025/14 (original text, partly superseded)", core="""Regulation 2025/14 as adopted. Articles on notification timing, testing and sanctions were amended by Amending Regulation 2026/03 (see that document); articles not amended remain as written here.

@@PAD@@

## Article 12 Initial notification
12.1 Financial entities shall submit an initial notification within 24 hours of classifying an incident as major.

## Article 13 Intermediate report
13.1 An intermediate report shall be submitted within 72 hours of the initial notification.

## Article 18 Testing
18.1 Entities designated as significant shall carry out threat-led penetration testing at least every 3 years.

## Article 24 Administrative fines
24.1 Fines may reach 1% of total annual turnover in the preceding financial year.

## Article 22 Record keeping
22.1 Incident records shall be kept for at least 5 years.
@@PAD@@
"""),
 dict(name="frr-amending-2026-03.md", title="Amending Regulation 2026/03 (in force from 2026-03-01)", core="""Amending Regulation 2026/03 amends Regulation 2025/14. In force from 1 March 2026.

@@PAD@@

## Article 1 Amendments
(a) Article 12.1 is replaced: "Financial entities shall submit an initial notification within 4 hours of classifying an incident as major."
(b) Article 18.1 is replaced: "Entities designated as significant shall carry out threat-led penetration testing at least every 2 years, counted from the date of the most recent completed test."
(c) Article 24.1 is replaced: "Fines may reach 2% of total annual turnover in the preceding financial year."
(d) Article 13.1 (72 hours) and Article 22.1 (record keeping) are unchanged.
@@PAD@@
"""),
 dict(name="frr-faq-regulator.md", title="Regulator FAQ on FRR (guidance, non-binding)", core="""Published guidance of the regulator. Non-binding; the regulation text prevails.

@@PAD@@

Q: What is the deadline for the initial notification? A: Older guidance says 24 hours; this FAQ was last updated before the 2026 amendment and has not been revised.
@@PAD@@
"""),
 dict(name="incident-log-mc77.md", title="Incident log MC-77", core="""Incident log for MC-77, a payment-gateway outage classified as major.

@@PAD@@

- 2026-05-08 08:55 incident classified as major by the duty officer.
- 2026-05-08 09:30 initial notification submitted to the authority.
- Intermediate report: to be submitted by the deadline in Article 13.1.
- Time zone: all times CET.
@@PAD@@
"""),
 dict(name="annual-report-fy2025.md", title="Annual report FY2025 (audited, extract)", core="""Marlowe Capital B.V. audited annual report for the financial year ended 31 December 2025, extract. Auditor's opinion unqualified.

@@PAD@@

Total annual turnover: EUR 412 million. (FY2024, for comparison: EUR 377 million.) Average headcount: 640.
@@PAD@@
"""),
 dict(name="annual-report-fy2024.md", title="Annual report FY2024 (audited, extract)", core="""Marlowe Capital B.V. audited annual report for the financial year ended 31 December 2024, extract.

@@PAD@@

Total annual turnover: EUR 377 million. Average headcount: 590.
@@PAD@@
"""),
 dict(name="compliance-wiki-authority.md", title="Compliance wiki: regulators", core="""Wiki page, last edited 2025-11-03. Convenience summary; per the wiki's own rule "board minutes override the wiki" for entity facts.

@@PAD@@

Competent authority for Marlowe: the National Financial Authority (NFA). Contact: the NFA incident desk.
@@PAD@@
"""),
 dict(name="board-minutes-2026-02.md", title="Board minutes, 12 February 2026", core="""Minutes of the board of Marlowe Capital B.V., 12 February 2026.

@@PAD@@

Item 5. Following the relocation of the registered office from Antwerp to Rotterdam, completed on 1 February 2026, the board noted that the competent authority for FRR purposes is now the Dutch Authority of Digital Finance (ADF). The registration with the ADF was filed on 9 February 2026. The wiki is to be corrected.
@@PAD@@
"""),
 dict(name="board-minutes-2026-04.md", title="Board minutes, 16 April 2026", core="""Minutes of the board of Marlowe Capital B.V., 16 April 2026.

@@PAD@@

Item 3. FRR accountable executive. The board resolved that Ingrid Solheim, Chief Risk Officer, is the accountable executive for FRR compliance with effect from 1 May 2026, replacing the earlier appointment of Tomas Wirth, who has taken the role of programme sponsor only.
@@PAD@@
"""),
 dict(name="board-minutes-2025-09.md", title="Board minutes, 18 September 2025", core="""Minutes of the board of Marlowe Capital B.V., 18 September 2025.

@@PAD@@

Item 4. The board appointed Tomas Wirth as the accountable executive for FRR compliance. Decision superseded by the board decision of 16 April 2026.
@@PAD@@
"""),
 dict(name="records-policy-v3.md", title="Records retention policy v3 (SUPERSEDED)", core="""Internal records retention policy v3, effective 2024-01-01; superseded by v4.

@@PAD@@

Incident records: retained for 5 years.
@@PAD@@
"""),
 dict(name="records-policy-v4.md", title="Records retention policy v4 (current)", core="""Internal records retention policy v4, effective 2026-01-01.

@@PAD@@

Where a statutory retention period exists and this policy sets a different one, the longer of the two applies. Incident records: retained for seven (7) years.
@@PAD@@
"""),
 dict(name="tpp-register-2026-03.md", title="Third-party provider register, 2026-03 snapshot", core="""ICT third-party register, snapshot of 31 March 2026. Subsequent changes are in the change log (applied in date order).

@@PAD@@

Critical ICT third-party providers on the register at the snapshot date: 14.
@@PAD@@
"""),
 dict(name="tpp-register-change-log.md", title="Third-party register change log", core="""Changes to the critical ICT third-party register after the 2026-03 snapshot.

@@PAD@@

- 2026-04-09: added Kestrel Payments (critical).
- 2026-04-22: removed Lowmoor Hosting (contract ended).
- 2026-05-14: added Tidewater Clearing (critical).
- 2026-06-03: added Ashgrove Identity (critical).
- 2026-06-18: removed Pinecrest Data (contract ended).
- 2026-06-25: added Verity Archive (non-critical; does not count towards the critical register).
@@PAD@@
"""),
 dict(name="pentest-history.md", title="Penetration testing history", core="""Record of threat-led penetration tests at Marlowe, maintained by the security office.

@@PAD@@

- Completed test: 2022-10-04 (previous cycle).
- Completed test: 2024-11-19 (most recent completed test).
- A test planned for 2026-02 was cancelled and did not complete.
@@PAD@@
"""),
 dict(name="peer-company-note-fjordbank.md", title="Peer comparison: Fjordbank NV", core="""Note comparing a peer. Not binding on Marlowe.

@@PAD@@

Fjordbank's turnover for FY2025 was EUR 530 million; its competent authority is the NFA; it keeps incident records for 6 years; its last penetration test was on 2025-03-11.
@@PAD@@
"""),
]
FILLER = [
 ("compliance-committee-notes-2026-01.md", "Compliance committee notes, January 2026"),
 ("compliance-committee-notes-2026-03.md", "Compliance committee notes, March 2026"),
 ("compliance-committee-notes-2026-05.md", "Compliance committee notes, May 2026"),
 ("training-plan-frr.md", "FRR training plan"),
 ("internal-audit-notes.md", "Internal audit notes"),
 ("risk-register-excerpt.md", "Risk register excerpt"),
 ("it-operations-review.md", "IT operations review"),
 ("faq-compliance-office.md", "Compliance office FAQ"),
 ("vendor-assessment-guidelines.md", "Vendor assessment guidelines"),
 ("incident-process-handbook.md", "Incident process handbook (generic)"),
 ("legal-updates-digest.md", "Legal updates digest"),
]
EXTRA_TOPICS = ["incident classification", "regulatory filings", "outsourcing assessments", "resilience testing", "supervisory contacts"]

PARTS = [
 dict(id="F1", kind="fact", desc="initial notification 4 hours (amended; not original 24)",
      all=[r"\b(4|four)\s*(\(4\)\s*)?hours?"]),
 dict(id="F2", kind="fact", desc="submit 2026-05-08 09:30 + 72h = 2026-05-11 09:30",
      all=[r"2026-05-11|11(th)?\s+May\s+2026|May\s+11(th)?,?\s+2026|11[./]05[./]2026"]),
 dict(id="F3", kind="fact", desc="2% x FY2025 EUR 412m = 8.24m (not 1%, not FY2024 377m)",
      all=[r"8[.,]24\s*(m\b|mn|million)|8[,.\s']?240[,.\s']?000"]),
 dict(id="F4", kind="fact", desc="ADF (board minutes override wiki's NFA)",
      all=[r"\bADF\b|Authority of Digital Finance"]),
 dict(id="F5", kind="fact", desc="7 years (policy v4 longer than statutory 5)",
      all=[r"\bseven\b|\b7[\s-]*(\(\w+\)\s*)?years?"]),
 dict(id="F6", kind="fact", desc="14 +3 added -2 removed = 15 (non-critical Verity ignored)",
      all=[r"(critical|provider|third)[^\n]{0,250}\b(15|fifteen)\b"]),
 dict(id="F7", kind="fact", desc="every 2 years from 2024-11-19 -> 2026-11-19 (not 2027)",
      all=[r"2026-11-19|19(th)?\s+Nov(ember)?\.?,?\s+2026|Nov(ember)?\.?\s+19(th)?,?\s+2026|19[./]11[./]2026"]),
 dict(id="F8", kind="fact", desc="Ingrid Solheim (April 2026 board), not Tomas Wirth",
      all=[r"Solheim"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# FRR compliance brief, Marlowe Capital

1. **Initial notification.** 4 hours from classification as major, as amended by Regulation 2026/03 (in force 1 March 2026). [frr-amending-2026-03.md] The original 24 hours [frr-2025-14-original.md] and the regulator FAQ [frr-faq-regulator.md] are outdated.
2. **MC-77 intermediate report.** 72 hours after the initial notification [frr-2025-14-original.md] submitted 2026-05-08 09:30 [incident-log-mc77.md]: due 2026-05-11 at 09:30.
3. **Maximum fine.** 2% of FY2025 turnover of EUR 412 million [annual-report-fy2025.md] under the amended Article 24.1 [frr-amending-2026-03.md] = EUR 8,240,000 (8.24 million).
4. **Competent authority.** The Dutch Authority of Digital Finance (ADF) after the move to Rotterdam [board-minutes-2026-02.md]; the wiki's NFA is outdated. [compliance-wiki-authority.md]
5. **Record retention.** 7 years: policy v4 sets seven years and the longer period applies over the statutory 5. [records-policy-v4.md] [frr-2025-14-original.md]
6. **Critical providers.** 14 at the 2026-03 snapshot [tpp-register-2026-03.md] plus 3 critical additions minus 2 removals in the change log = 15. [tpp-register-change-log.md]
7. **Next pen test.** Every 2 years from the last completed test on 2024-11-19 [pentest-history.md], per the amended Article 18.1 [frr-amending-2026-03.md]: due 2026-11-19.
8. **Accountable executive.** Ingrid Solheim, CRO, from 1 May 2026 per the 16 April 2026 board decision. [board-minutes-2026-04.md]
"""

BAD_BRIEF = """1. 24 hours. 2. 2026-05-11 09:30. 3. EUR 4,120,000. 4. NFA. 5. 5 years. 6. 14. 7. 2027-11-19. 8. Tomas Wirth. [frr-faq-regulator.md] [compliance-wiki-authority.md] [records-policy-v3.md] [board-minutes-2025-09.md] [tpp-register-2026-03.md]"""
