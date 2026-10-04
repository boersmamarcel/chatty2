"""Inventory helpers."""

def count_inclusive_inventory(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def ceil_div_inventory(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def is_leap_inventory(y):
    """True for Gregorian leap years."""
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)

def running_total_inventory(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def tidy_name_inventory(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def clamp_inventory(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def weighted_sum_inventory(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    return sum(v * w for v, w in zip(vals, weights))

def last_n_inventory(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def grade_inventory(score):
    """'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'."""
    return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'

def word_count_inventory(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def starts_with_any_inventory(s, prefixes):
    """True when s starts with at least one of the prefixes (False for no prefixes)."""
    return any(s.startswith(p) for p in prefixes)

def dedupe_inventory(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out
