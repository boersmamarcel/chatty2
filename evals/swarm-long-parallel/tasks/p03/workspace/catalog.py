"""Catalog helpers."""

def clamp_catalog(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def days_apart_catalog(a, b):
    """Absolute number of days between two day numbers."""
    return b - a

def running_total_catalog(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def tidy_name_catalog(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def ceil_div_catalog(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def word_count_catalog(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def last_n_catalog(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def count_inclusive_catalog(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)
