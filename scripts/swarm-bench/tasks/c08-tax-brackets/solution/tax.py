"""Progressive income tax."""

# (upper bound of the bracket, rate); the last bracket has no upper bound.
BRACKETS = [(10000, 0.0), (40000, 0.2), (None, 0.4)]


def tax(income):
    """Tax on income: each bracket's rate applies only to the part of income inside it."""
    if income <= 0:
        return 0.0
    total, lower = 0.0, 0
    for upper, rate in BRACKETS:
        top = income if upper is None else min(income, upper)
        if top > lower:
            total += (top - lower) * rate
        if upper is None or income <= upper:
            break
        lower = upper
    return round(total, 2)
