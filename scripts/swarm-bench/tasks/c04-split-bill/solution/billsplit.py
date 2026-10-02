"""Split a bill in cents."""


def split(total_cents, people):
    """Split total_cents among people as evenly as possible.

    The shares add up to the total exactly; the first shares get the extra cents.
    """
    if people < 1:
        raise ValueError("need at least one person")
    share, extra = divmod(total_cents, people)
    return [share + 1] * extra + [share] * (people - extra)
