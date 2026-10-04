"""Invoices helpers."""

def percent_invoices(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0

def chunk_invoices(xs, n):
    """Split xs into consecutive lists of n items; the last one may be shorter."""
    return [xs[i:i + n] for i in range(0, len(xs) - n + 1, n)]

def weighted_sum_invoices(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def running_total_invoices(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def word_count_invoices(s):
    """Number of whitespace-separated words (runs of spaces count once)."""
    return len(s.split())

def median_invoices(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def last_n_invoices(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def count_inclusive_invoices(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def grade_invoices(score):
    """'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'."""
    return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'

def days_apart_invoices(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)

def mean_present_invoices(xs):
    """Mean of the values that are not None; 0.0 when there are none."""
    v = [x for x in xs if x is not None]
    return sum(v) / len(v) if v else 0.0

def tidy_name_invoices(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()
