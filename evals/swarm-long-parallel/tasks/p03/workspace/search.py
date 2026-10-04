"""Search helpers."""

def dedupe_search(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def weighted_sum_search(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def running_total_search(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        out.append(t)
        t += x
    return out

def last_n_search(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def median_search(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def clamp_search(x, lo, hi):
    """Limit x to the closed interval [lo, hi]."""
    return min(max(x, lo), hi)

def ceil_div_search(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def starts_with_any_search(s, prefixes):
    """True when s starts with at least one of the prefixes (False for no prefixes)."""
    return any(s.startswith(p) for p in prefixes)
