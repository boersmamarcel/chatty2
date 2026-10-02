"""Business days between two dates."""

import datetime


def business_days(start, end, holidays=()):
    """Working days from start to end inclusive: Monday to Friday, minus holidays."""
    if end < start:
        return 0
    count = 0
    day = start
    while day <= end:
        if day.weekday() < 5:
            count += 1
        day += datetime.timedelta(days=1)
    for h in holidays:
        if start <= h <= end:
            count -= 1
    return count
