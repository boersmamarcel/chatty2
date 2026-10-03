"""gradebook: course grade calculation for one course section.

Modules:

    models   -- students, assessments, marks and the EXCUSED marker
    loader   -- CSV import of the roster, the assessments and the marks
    late     -- late-submission penalties
    policy   -- category weights and the drop-lowest rule
    grading  -- category and course percentages, letter grades
    report   -- class statistics and the plain-text grade report
    cli      -- command line entry point (python3 -m gradebook.cli)
"""

__version__ = "0.4.1"
