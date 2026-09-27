import datetime


def _parse(text):
    return datetime.datetime.strptime(text, "%Y-%m-%d").date()


def days_between(start, end, business_only=False):
    """Days from start to end (ISO dates); only weekdays when business_only."""
    a, b = _parse(start), _parse(end)
    if not business_only:
        return (b - a).days
    sign = 1
    if b < a:
        a, b, sign = b, a, -1
    count = sum(1 for i in range((b - a).days)
                if (a + datetime.timedelta(days=i)).weekday() < 5)
    return sign * count
