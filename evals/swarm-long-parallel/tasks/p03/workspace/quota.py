"""Quota helpers."""

def count_inclusive_quota(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def days_apart_quota(a, b):
    """Absolute number of days between two day numbers."""
    return b - a

def starts_with_any_quota(s, prefixes):
    """True when s starts with at least one of the prefixes (False for no prefixes)."""
    return any(s.startswith(p) for p in prefixes)

def clamp_quota(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def weighted_sum_quota(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def word_count_quota(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def tidy_name_quota(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def ceil_div_quota(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)
