"""Reports helpers."""

def word_count_reports(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def median_reports(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def count_inclusive_reports(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def clamp_reports(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def ceil_div_reports(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def last_n_reports(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def weighted_sum_reports(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    return sum(v * w for v, w in zip(vals, weights))

def percent_reports(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0
