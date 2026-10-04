"""Quota helpers."""

def count_inclusive_quota(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo)

def chunk_quota(xs, n):
    """Split xs into consecutive lists of n items; the last one may be shorter."""
    return [xs[i:i + n] for i in range(0, len(xs), n)]

def starts_with_any_quota(s, prefixes):
    """True when s starts with at least one of the prefixes (False for no prefixes)."""
    return any(s.startswith(p) for p in prefixes)

def days_apart_quota(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)

def percent_quota(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0

def tidy_name_quota(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def running_total_quota(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def weighted_sum_quota(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def last_n_quota(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def median_quota(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def dedupe_quota(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def mean_present_quota(xs):
    """Mean of the values that are not None; 0.0 when there are none."""
    v = [x for x in xs if x is not None]
    return sum(v) / len(v) if v else 0.0
