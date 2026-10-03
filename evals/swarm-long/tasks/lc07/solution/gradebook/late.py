"""Late-submission penalties.

Syllabus, section 4.2: work handed in after the deadline loses 10 % of the
assessment's maximum points for every day, or part of a day, that it is
late. The penalty never exceeds 50 % of the maximum points, and a mark never
drops below zero because of it. A student with an extension of N days has
their deadline moved N whole days later.
"""

import datetime

from .models import EXCUSED

RATE_PER_DAY = 0.10
MAX_PENALTY_DAYS = 5

ONE_DAY = datetime.timedelta(days=1)


def effective_due(due, extension_days=0):
    """The deadline after an extension of `extension_days` whole days."""
    if due is None:
        return None
    if extension_days < 0:
        raise ValueError("extension_days must not be negative: %r" % (extension_days,))
    return due + extension_days * ONE_DAY


def days_late(due, submitted):
    """Number of days `submitted` is after `due` (0 when on time).

    Either value may be None, which means "on time".
    """
    if due is None or submitted is None:
        return 0
    delta = submitted - due
    if delta <= datetime.timedelta(0):
        return 0
    days = delta.days
    if delta - datetime.timedelta(days=days) > datetime.timedelta(0):
        days += 1
    return days


def apply_late(points, max_points, due, submitted, extension_days=0):
    """Points after the late penalty.

    None (missing) and EXCUSED are returned unchanged.
    """
    if points is None or points is EXCUSED:
        return points
    days = min(days_late(effective_due(due, extension_days), submitted), MAX_PENALTY_DAYS)
    if days == 0:
        return points
    penalty = max_points * RATE_PER_DAY * days
    return max(0.0, points - penalty)


def describe(due, submitted, extension_days=0):
    """Short human text for a report cell, e.g. 'on time' or '2 days late'."""
    days = days_late(effective_due(due, extension_days), submitted)
    if days == 0:
        return "on time"
    if days == 1:
        return "1 day late"
    return "%d days late" % days
