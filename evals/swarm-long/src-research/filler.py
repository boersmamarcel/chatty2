"""Deterministic filler-text engine for the research-write long tasks.

Produces realistic, varied but fact-free corporate prose (meeting notes,
policy clauses, status updates, FAQs, glossaries) from templates and word
banks. Everything is driven by a random.Random passed in by the caller.
"""

FIRST = ["Aino", "Bram", "Carla", "Dmitri", "Elena", "Farid", "Greta", "Hugo", "Ilse", "Jonas",
         "Katya", "Lars", "Mina", "Nils", "Olga", "Pedro", "Quinn", "Rosa", "Sven", "Tamsin",
         "Uri", "Vera", "Wim", "Xenia", "Yusuf", "Zora", "Anders", "Beatrix", "Cormac", "Dalia",
         "Emil", "Fenna", "Gideon", "Hana", "Isak", "Jelena", "Koen", "Livia", "Mateo", "Noor"]
LAST = ["Aalberts", "Bakker", "Castellan", "Dvorak", "Eriksen", "Fontaine", "Grosz", "Haugen",
        "Ivanova", "Jansen", "Kowalski", "Lindqvist", "Marchetti", "Nakamura", "Ostrom", "Petrov",
        "Quast", "Rinaldi", "Sorensen", "Thijssen", "Underhill", "Vandermeer", "Weiss", "Yamada",
        "Zielinski", "Abbasi", "Brandvold", "Corrigan", "Delacroix", "Eklund"]
TEAMS = ["the platform group", "the finance team", "the operations group", "the legal desk",
         "the security guild", "the programme office", "the procurement team", "the quality group",
         "the data team", "the facilities team", "the communications team", "the steering group",
         "the architecture board", "the support organisation", "the risk committee"]
TOPICS = ["onboarding checklist", "reporting cadence", "access review", "document control",
          "training plan", "supplier scorecard", "budget tracking", "risk register",
          "change calendar", "escalation path", "vendor invoices", "archive migration",
          "dashboard refresh", "template library", "audit trail", "capacity forecast",
          "workshop schedule", "tooling inventory", "style guide", "retention schedule",
          "sign-off workflow", "handover notes", "meeting rhythm", "knowledge base",
          "naming conventions", "backlog grooming", "status template", "readiness review"]
ACTIONS = ["circulate a short summary", "draft a revised outline", "confirm the owner list",
           "collect comments from the wider group", "schedule a follow-up session",
           "update the shared tracker", "review the existing wording", "prepare a comparison table",
           "check the numbering of the annexes", "align the terminology with the glossary",
           "ask the neighbouring team for input", "close the stale items", "archive the old drafts",
           "prepare a one-page overview", "validate the links in the index", "book a room for the workshop"]
DEPS = ["the next release of the shared template", "availability of the reviewers",
        "a decision from the steering group", "the outcome of the quarterly planning round",
        "feedback from the regional offices", "completion of the tooling upgrade",
        "the holiday calendar", "the updated organisation chart", "budget confirmation",
        "access to the shared drive"]
CADENCE = ["weekly", "fortnightly", "monthly", "quarterly", "end-of-sprint", "mid-cycle"]
HEDGES = ["It was noted that", "The group observed that", "Several members pointed out that",
          "There was general agreement that", "It was recalled that",
          "The chair reminded attendees that", "One participant suggested that"]
CHAIR = ["The chair", "The secretary", "The project lead", "The group", "The meeting"]
ADJ = ["pragmatic", "incremental", "lightweight", "well-documented", "transparent", "repeatable",
       "conservative", "proportionate", "pragmatic", "consistent"]
OBLIG = ["maintain accurate records of", "make available on request", "keep confidential",
         "review on a regular basis", "notify the other party of material changes to",
         "retain appropriate evidence of", "document and communicate", "ensure suitable training for",
         "assign a named owner for", "restrict access to"]
OBJ = ["working papers", "correspondence", "supporting schedules", "administrative procedures",
       "internal guidance notes", "operating instructions", "contact lists", "distribution lists",
       "interim drafts", "meeting records", "reference material", "routine reports"]
GLOSS = [("Working day", "a day other than a Saturday, Sunday or public holiday at the relevant site"),
         ("Business owner", "the person accountable for the outcome of a process, not its day-to-day execution"),
         ("Reviewer", "a person who checks a document against its stated criteria before sign-off"),
         ("Baseline", "the agreed reference state against which later changes are described"),
         ("Artefact", "any document, file or record produced as part of the work"),
         ("Stakeholder", "anyone materially affected by, or able to influence, the work"),
         ("Escalation", "the act of raising an unresolved matter to the next level of authority"),
         ("Register", "a maintained list of items together with their status and owner"),
         ("Record", "information created or received that is kept as evidence of an activity"),
         ("Draft", "a version that has not yet been approved for use"),
         ("Distribution list", "the set of recipients who receive a document by default"),
         ("Handover", "the transfer of responsibility from one team or person to another")]
FAQQ = [("Where do I find the latest template?", "The latest template is kept in the shared library; older copies are archived and should not be reused."),
        ("Who may approve a minor wording change?", "The document owner may approve editorial changes that do not alter meaning; anything else goes to the reviewers."),
        ("How are conflicting instructions handled?", "Raise the conflict with the document owner, who records the decision in the change log."),
        ("Can a meeting be held without the chair?", "Yes, provided a deputy has been named in advance and the minutes record this."),
        ("How long are drafts kept?", "Drafts are kept until the final version is published and then follow the normal archive routine."),
        ("What if an owner leaves the organisation?", "The line manager nominates a successor within the usual handover period."),
        ("Is there a standard subject line for status mails?", "Yes: the programme name, the word Status and the reporting period."),
        ("Who maintains the distribution lists?", "The programme office maintains them and publishes changes in the weekly digest.")]
SPECIAL = ["a pilot", "a dry run", "a desk review", "a walkthrough", "a short survey", "a retrospective"]
PLACES = ["the main conference room", "the second-floor meeting room", "a video call", "the project room",
          "the training room", "the annex building"]
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September",
          "October", "November", "December"]


def person(r):
    return "%s %s" % (r.choice(FIRST), r.choice(LAST))


def date_str(r, years=(2022, 2023, 2024)):
    return "%d %s %d" % (r.randint(1, 28), r.choice(MONTHS), r.choice(years))


def sentence(r):
    t = r.randint(0, 11)
    topic = r.choice(TOPICS)
    if t == 0:
        return "%s reviewed the %s and agreed to %s before the next %s review." % (
            r.choice(TEAMS).capitalize(), topic, r.choice(ACTIONS), r.choice(CADENCE))
    if t == 1:
        return "%s the %s remains dependent on %s." % (r.choice(HEDGES), topic, r.choice(DEPS))
    if t == 2:
        return "%s asked %s to %s and report back at the %s check-in." % (
            r.choice(CHAIR), person(r), r.choice(ACTIONS), r.choice(CADENCE))
    if t == 3:
        return "The %s approach to the %s was considered sufficient for now, subject to %s." % (
            r.choice(ADJ), topic, r.choice(DEPS))
    if t == 4:
        return "No objections were raised to the proposal on the %s; %s will %s." % (
            topic, r.choice(TEAMS), r.choice(ACTIONS))
    if t == 5:
        return "In the interim, %s will keep using the existing %s and note any gaps." % (
            r.choice(TEAMS), topic)
    if t == 6:
        return "%s that the %s should stay %s and avoid adding new steps without a clear owner." % (
            r.choice(HEDGES), topic, r.choice(ADJ))
    if t == 7:
        return "A short %s will be used to test the revised %s before it is rolled out more widely." % (
            r.choice(SPECIAL).replace("a ", "", 1), topic)
    if t == 8:
        return "%s offered to %s, which was welcomed by the group." % (person(r), r.choice(ACTIONS))
    if t == 9:
        return "The wording of the %s section was left unchanged pending %s." % (topic, r.choice(DEPS))
    if t == 10:
        return "Members agreed that the %s would be reviewed again at the %s meeting, with %s coordinating." % (
            topic, r.choice(CADENCE), person(r))
    return "For completeness, %s confirmed that the %s had been filed in the usual place." % (
        person(r), topic)


def para(r, n=None):
    return " ".join(sentence(r) for _ in range(n or r.randint(3, 5)))


def sec_meeting(r):
    out = ["### Meeting notes, %s" % date_str(r), "",
           "Location: %s. Attendees: %s." % (r.choice(PLACES), ", ".join(person(r) for _ in range(r.randint(3, 6)))), ""]
    for i in range(r.randint(3, 4)):
        out += ["**Item %d: %s**" % (i + 1, r.choice(TOPICS).capitalize()), "", para(r), ""]
    out.append("Actions:")
    for _ in range(r.randint(2, 4)):
        out.append("- %s to %s." % (person(r), r.choice(ACTIONS)))
    out.append("")
    return "\n".join(out)


def sec_clauses(r):
    n = r.randint(2, 9)
    out = ["### General provisions, part %d" % n, ""]
    for m in range(1, r.randint(5, 8)):
        out.append("%d.%d Each party shall %s its %s and shall %s where reasonably requested. %s" % (
            n, m, r.choice(OBLIG), r.choice(OBJ), r.choice(OBLIG).replace("of ", "of its ", 1) + " " + r.choice(OBJ),
            sentence(r)))
        out.append("")
    return "\n".join(out)


def sec_status(r):
    out = ["### Status update, %s" % date_str(r), "",
           "Overall: %s." % r.choice(["on track", "on track with minor risks", "slightly behind on documentation", "steady"]), ""]
    for t in r.sample(TOPICS, 4):
        out.append("- %s: %s" % (t.capitalize(), sentence(r)))
    out += ["", para(r, 3), ""]
    return "\n".join(out)


def sec_faq(r):
    out = ["### Frequently asked questions", ""]
    for q, a in r.sample(FAQQ, 4):
        out += ["**%s**" % q, a + " " + sentence(r), ""]
    return "\n".join(out)


def sec_gloss(r):
    out = ["### Glossary of working terms", ""]
    for t, d in r.sample(GLOSS, 5):
        out.append("- **%s**: %s." % (t, d))
    out += ["", para(r, 2), ""]
    return "\n".join(out)


SECTIONS = [sec_meeting, sec_clauses, sec_status, sec_faq, sec_gloss, sec_meeting, sec_clauses, sec_status]


def pad(r, words, extra_topics=()):
    """Return filler markdown of at least `words` words."""
    saved = None
    if extra_topics:
        saved = list(TOPICS)
        TOPICS.extend(extra_topics)
    try:
        out, n = [], 0
        while n < words:
            s = r.choice(SECTIONS)(r)
            out.append(s)
            n += len(s.split())
        return "\n".join(out)
    finally:
        if saved is not None:
            TOPICS[:] = saved
