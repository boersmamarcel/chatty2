"""Plain records used across the gradebook.

Points are floats. The points of a mark can be:

* a number -- the points earned (may exceed max_points: extra credit),
* None     -- missing work; it counts as 0 points,
* EXCUSED  -- the student is excused; the assessment is left out of the
              category entirely (neither earned nor possible points).
"""

import datetime


class _Excused(object):
    """Singleton marker for an excused assessment."""

    __slots__ = ()

    def __repr__(self):
        return "EXCUSED"

    def __reduce__(self):
        return "EXCUSED"


EXCUSED = _Excused()


class Student(object):
    """A student on the roster."""

    def __init__(self, student_id, name, section=""):
        self.student_id = student_id
        self.name = name
        self.section = section

    def __repr__(self):
        return "Student(%r, %r)" % (self.student_id, self.name)


class Assessment(object):
    """One gradable item (homework 3, the midterm, ...).

    `due` is a datetime (the deadline) or None; `category` names a
    `policy.CategoryPolicy`.
    """

    def __init__(self, key, category, max_points, due=None):
        if max_points <= 0:
            raise ValueError("max_points must be positive: %r" % (max_points,))
        self.key = key
        self.category = category
        self.max_points = float(max_points)
        self.due = due

    def __repr__(self):
        return "Assessment(%r, %r, %r)" % (self.key, self.category, self.max_points)


class Mark(object):
    """A student's result on one assessment.

    `submitted` is the datetime the work was handed in (None: not recorded,
    treated as on time). `extension_days` is a whole number of days the
    deadline was extended for this student.
    """

    def __init__(self, student_id, assessment_key, points, submitted=None, extension_days=0):
        self.student_id = student_id
        self.assessment_key = assessment_key
        self.points = points
        self.submitted = submitted
        self.extension_days = extension_days

    @property
    def excused(self):
        return self.points is EXCUSED

    def __repr__(self):
        return "Mark(%r, %r, %r)" % (self.student_id, self.assessment_key, self.points)


def parse_datetime(text):
    """Parse 'YYYY-MM-DD' (meaning 23:59 that day) or 'YYYY-MM-DD HH:MM'."""
    text = text.strip()
    if len(text) == 10:
        day = datetime.datetime.strptime(text, "%Y-%m-%d")
        return day.replace(hour=23, minute=59)
    return datetime.datetime.strptime(text, "%Y-%m-%d %H:%M")
