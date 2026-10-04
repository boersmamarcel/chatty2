"""Sync helpers."""

def grade_sync(score):
    """'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'."""
    return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'

def ceil_div_sync(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def mean_present_sync(xs):
    """Mean of the values that are not None; 0.0 when there are none."""
    v = [x for x in xs if x is not None]
    return sum(v) / len(v) if v else 0.0

def is_leap_sync(y):
    """True for Gregorian leap years."""
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)

def word_count_sync(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split(' '))

def running_total_sync(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def tidy_name_sync(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def dedupe_sync(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def count_inclusive_sync(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def starts_with_any_sync(s, prefixes):
    """True when s starts with at least one of the prefixes (False for no prefixes)."""
    return any(s.startswith(p) for p in prefixes)

def last_n_sync(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def days_apart_sync(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)
