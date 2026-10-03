"""lr04: product launch readiness (Northwind Notes 5.0)."""

TITLE = "Product launch readiness: Northwind Notes 5.0"
INTRO = ("Northwind Notes 5.0 is heading for launch. The launch manager needs a readiness brief that settles dates, "
         "budget, pricing, quality and ownership from the plans, decision records, minutes, reports and runbooks in docs/.")
QUESTIONS = [
    "On what date is the launch of Northwind Notes 5.0 (as decided most recently)?",
    "What is the total approved budget in USD, including any released contingency?",
    "How many P0 bugs remain open at the go/no-go review, after the triage note's adjustments?",
    "What is the annual per-seat price in USD under the approved pricing (monthly list price with the annual-billing discount applied)?",
    "What is the minimum supported iOS version at launch?",
    "At what time in UTC does the press embargo lift on launch day?",
    "How many concurrent users must infrastructure be able to support at launch (forecast with the mandated headroom)?",
    "Who is the rollback decision owner on launch day under the current runbook?",
]

DOCS = [
 dict(name="launch-plan-v1.md", title="Launch plan v1 (SUPERSEDED)", core="""Launch plan v1, written 2026-03-10. Superseded by the decision of the steering committee on 2026-05-12 (see steering minutes) and by launch plan v2.

@@PAD@@

Planned launch date: 2026-06-02. Code freeze: 2026-05-19. Approved budget: USD 1,200,000. Pricing: USD 12 per seat per month list.
@@PAD@@
"""),
 dict(name="launch-plan-v2.md", title="Launch plan v2", core="""Launch plan v2, 2026-05-14. Current plan; implements the decisions of the steering committee of 2026-05-12.

@@PAD@@

Code freeze: 2026-06-09. After code freeze there is a soak period of 14 days (production-like environment, no feature changes) and launch happens on the first day after the soak period ends. Press embargo and launch communications are scheduled relative to launch day (see comms plan).
@@PAD@@
"""),
 dict(name="steering-minutes-2026-05-12.md", title="Steering committee minutes, 12 May 2026", core="""Steering committee minutes, 12 May 2026.

@@PAD@@

Decision 1. The launch slips from the original date. The new code freeze is 9 June 2026. The committee confirmed that the 14-day soak period applies in full and that launch follows the soak without further delay.
Decision 2. The finance lead's contingency request was approved (see CFO email).
Decision 3. Pricing proposal v3 approved (see pricing council note).
@@PAD@@
"""),
 dict(name="cfo-email-contingency.md", title="Email from the CFO: contingency release", core="""From: CFO. To: Programme office. Date: 2026-05-13. Subject: Northwind Notes 5.0 contingency.

@@PAD@@

Following the steering committee, I confirm release of the USD 150,000 contingency on top of the previously approved programme budget. No other changes to the budget.
@@PAD@@
"""),
 dict(name="budget-approval-2026-02.md", title="Budget approval record, February 2026", core="""Finance approval record for Northwind Notes 5.0, 2026-02-24.

@@PAD@@

Approved programme budget: USD 1,200,000. Contingency of USD 150,000 held back and not part of the approved programme budget until released by the CFO in writing.
@@PAD@@
"""),
 dict(name="bug-report-2026-06-05.md", title="Quality report: open bugs as of 2026-06-05", core="""Quality report from the QA lead, 2026-06-05, listing open bugs by priority.

@@PAD@@

Open P0 bugs: 23. Open P1 bugs: 61. The P0 list was taken before triage; see the triage note of 2026-06-08.
@@PAD@@
"""),
 dict(name="triage-note-2026-06-08.md", title="Bug triage note, 8 June 2026", core="""Triage meeting note, 2026-06-08, adjusting the P0 count from the 2026-06-05 report.

@@PAD@@

Four P0 bugs (BUG-1182, BUG-1190, BUG-1204, BUG-1211) were downgraded to P1. Two P0 bugs (BUG-1175 and BUG-1199) were found to be duplicates and closed. No P0 bugs were added after the report. All other P0 bugs remain open and block launch.
@@PAD@@
"""),
 dict(name="pricing-council-v2.md", title="Pricing proposal v2 (SUPERSEDED)", core="""Pricing council proposal v2, 2026-04-02. Superseded by v3.

@@PAD@@

List price: USD 12 per seat per month. Annual billing discount: 15%.
@@PAD@@
"""),
 dict(name="pricing-council-v3.md", title="Pricing proposal v3 (approved 2026-05-12)", core="""Pricing council proposal v3, approved by the steering committee on 2026-05-12.

@@PAD@@

List price: USD 14 per seat per month. Annual billing: customers who pay annually receive a 15% discount on twelve months of the monthly list price. Prices exclude tax.
@@PAD@@
"""),
 dict(name="adr-014-minimum-os.md", title="ADR-014: minimum OS versions", core="""Architecture decision record 014, dated 2026-04-20. Supersedes the minimum-OS line in the product requirements document (PRD).

@@PAD@@

Decision: the minimum supported iOS version at launch is iOS 16.0. Minimum supported Android is Android 10. Rationale: the share of devices on older versions is below 2% and the new editor needs APIs introduced in iOS 16.
@@PAD@@
"""),
 dict(name="prd-v4.md", title="Product requirements document v4", core="""PRD v4, 2026-02-02. Parts of this document have been superseded by later decision records; where a later ADR conflicts, the ADR governs.

@@PAD@@

Platforms: iOS (minimum iOS 15.0), Android (minimum Android 9), web. The editor must support offline mode.
@@PAD@@
"""),
 dict(name="comms-plan.md", title="Launch communications plan", core="""Communications plan, 2026-05-20.

@@PAD@@

The press embargo lifts at 09:00 America/Los_Angeles on launch day. Note that Los Angeles observes daylight saving time in June. Launch-day blog post and store listings go live at the same moment.
@@PAD@@
"""),
 dict(name="capacity-forecast.md", title="Capacity forecast for launch", core="""Infrastructure capacity forecast, 2026-05-27.

@@PAD@@

Forecast peak concurrent users in the first week after launch: 120,000. The architecture board's headroom rule requires provisioning for 1.5 times the forecast peak.
@@PAD@@
"""),
 dict(name="launch-runbook-v2.md", title="Launch day runbook v2 (SUPERSEDED)", core="""Launch runbook v2, 2026-04-15. Superseded by v3.

@@PAD@@

Rollback decision owner: Lee Marsh (head of engineering). Backup: the on-call incident commander.
@@PAD@@
"""),
 dict(name="launch-runbook-v3.md", title="Launch day runbook v3 (current)", core="""Launch runbook v3, 2026-06-10.

@@PAD@@

Rollback decision owner: Dana Okafor (VP Engineering), who took over launch responsibility after the reorganisation. Backup: Lee Marsh. The rollback decision may be taken without further approval during the first 48 hours.
@@PAD@@
"""),
 dict(name="competitor-notes-lumenpad.md", title="Competitor notes: LumenPad 3", core="""Notes on a competing product.

@@PAD@@

LumenPad 3 launched on 2026-05-28, priced at USD 11 per seat per month, minimum iOS 17, with a press embargo that lifted at 06:00 PT.
@@PAD@@
"""),
]
FILLER = [
 ("marketing-sync-notes-2026-04.md", "Marketing sync notes, April 2026"),
 ("marketing-sync-notes-2026-05.md", "Marketing sync notes, May 2026"),
 ("engineering-standup-digest.md", "Engineering stand-up digest"),
 ("qa-process-handbook.md", "QA process handbook"),
 ("support-readiness-notes.md", "Support readiness notes"),
 ("localisation-plan.md", "Localisation plan"),
 ("accessibility-review.md", "Accessibility review notes"),
 ("beta-programme-notes.md", "Beta programme notes"),
 ("faq-launch-office.md", "Launch office FAQ"),
 ("store-listing-guidelines.md", "App store listing guidelines"),
 ("sales-enablement-notes.md", "Sales enablement notes"),
]
EXTRA_TOPICS = ["store listings", "beta feedback", "release candidates", "crash reporting", "onboarding flow"]

PARTS = [
 dict(id="F1", kind="fact", desc="freeze 2026-06-09 + 14d soak -> launch 2026-06-23 (not 2026-06-02)",
      all=[r"2026-06-23|23(rd)?\s+June\s+2026|June\s+23(rd)?,?\s+2026|23[./]06[./]2026"]),
 dict(id="F2", kind="fact", desc="1.2M + 150k released = 1.35M",
      all=[r"1[.,]35\s*(m\b|mn|million)|1[,.\s']?350[,.\s']?000"]),
 dict(id="F3", kind="fact", desc="23 - 4 downgraded - 2 duplicates = 17",
      all=[r"P0[^\n]{0,250}\b(17|seventeen)\b|\b(17|seventeen)\b\s*(open\s*)?(P0|bugs?|blockers?)"]),
 dict(id="F4", kind="fact", desc="14 x 12 x 0.85 = 142.80 (not 122.40)",
      all=[r"142[.,]8"]),
 dict(id="F5", kind="fact", desc="iOS 16 (ADR-014 beats PRD iOS 15)",
      all=[r"iOS\s*16"]),
 dict(id="F6", kind="fact", desc="09:00 PDT = 16:00 UTC (not 17:00)",
      all=[r"\b16:00|\b4\s*pm\s*UTC|\b16\.00"]),
 dict(id="F7", kind="fact", desc="120,000 x 1.5 = 180,000",
      all=[r"180[,.\s']?000|180\s*k\b"]),
 dict(id="F8", kind="fact", desc="Dana Okafor (runbook v3 not Lee Marsh)",
      all=[r"Okafor"]),
 dict(id="C", kind="citations", desc="cites >=5 corpus files", min_files=5),
]

BRIEF = """# Northwind Notes 5.0 readiness brief

1. **Launch date.** Code freeze 9 June 2026 [steering-minutes-2026-05-12.md] plus the 14-day soak period [launch-plan-v2.md]; launch is on 23 June 2026. The original 2 June date is superseded. [launch-plan-v1.md]
2. **Budget.** USD 1,200,000 approved [budget-approval-2026-02.md] plus the USD 150,000 contingency released by the CFO [cfo-email-contingency.md] = USD 1,350,000.
3. **Open P0 bugs.** 23 reported [bug-report-2026-06-05.md] minus 4 downgraded and 2 duplicates [triage-note-2026-06-08.md] = 17.
4. **Annual price per seat.** USD 14 monthly list (v3) [pricing-council-v3.md]: 14 x 12 x 0.85 = USD 142.80 per seat per year.
5. **Minimum iOS.** iOS 16.0 per ADR-014 [adr-014-minimum-os.md]; the PRD's iOS 15 is superseded. [prd-v4.md]
6. **Embargo.** 09:00 America/Los_Angeles on launch day [comms-plan.md]; in June that is PDT (UTC-7), so 16:00 UTC.
7. **Concurrent users.** Forecast 120,000 x headroom 1.5 = 180,000. [capacity-forecast.md]
8. **Rollback owner.** Dana Okafor under runbook v3. [launch-runbook-v3.md] (v2 named Lee Marsh [launch-runbook-v2.md].)
"""

BAD_BRIEF = """1. 2026-06-02. 2. USD 1,200,000. 3. 23. 4. USD 122.40. 5. iOS 15. 6. 17:00 UTC. 7. 120,000. 8. Lee Marsh. [launch-plan-v1.md] [budget-approval-2026-02.md] [pricing-council-v2.md] [prd-v4.md] [launch-runbook-v2.md]"""
