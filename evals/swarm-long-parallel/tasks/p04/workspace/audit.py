"""Audit helpers."""

def word_count_audit(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def last_n_audit(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def grade_audit(score):
    """'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'."""
    return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'

def is_leap_audit(y):
    """True for Gregorian leap years."""
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)

def days_apart_audit(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)

def clamp_audit(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def median_audit(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def chunk_audit(xs, n):
    """Split xs into consecutive lists of n items; the last one may be shorter."""
    return [xs[i:i + n] for i in range(0, len(xs), n)]

def count_inclusive_audit(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def running_total_audit(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        out.append(t)
        t += x
    return out

def dedupe_audit(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def weighted_sum_audit(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))
