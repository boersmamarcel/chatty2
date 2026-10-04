"""Payroll helpers."""

def tidy_name_payroll(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def weighted_sum_payroll(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def mean_present_payroll(xs):
    """Mean of the values that are not None; 0.0 when there are none."""
    v = [x for x in xs if x is not None]
    return sum(v) / len(v) if v else 0.0

def last_n_payroll(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def clamp_payroll(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def dedupe_payroll(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def count_inclusive_payroll(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo)

def ceil_div_payroll(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)
