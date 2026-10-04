"""Inventory helpers."""

def percent_inventory(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0

def word_count_inventory(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def running_total_inventory(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def median_inventory(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def chunk_inventory(xs, n):
    """Split xs into consecutive lists of n items; the last one may be shorter."""
    return [xs[i:i + n] for i in range(0, len(xs) - n + 1, n)]

def days_apart_inventory(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)

def mean_present_inventory(xs):
    """Mean of the values that are not None; 0.0 when there are none."""
    v = [x for x in xs if x is not None]
    return sum(v) / len(v) if v else 0.0

def count_inclusive_inventory(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)
