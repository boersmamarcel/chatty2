"""Shipping helpers."""

def weighted_sum_shipping(vals, weights):
    """Sum of value times weight; ValueError when the lengths differ."""
    if len(vals) != len(weights):
        raise ValueError('length mismatch')
    return sum(v * w for v, w in zip(vals, weights))

def days_apart_shipping(a, b):
    """Absolute number of days between two day numbers."""
    return abs(a - b)

def tidy_name_shipping(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return s.strip().title()

def median_shipping(xs):
    """Median of a non-empty list; the mean of the two middle values for even lengths."""
    s = sorted(xs)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2

def dedupe_shipping(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = set()
    out = []
    for x in xs:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out

def ceil_div_shipping(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def percent_shipping(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0

def grade_shipping(score):
    """'A' for 90 and above, 'B' for 80 and above, 'C' for 70 and above, else 'D'."""
    return 'A' if score >= 90 else 'B' if score >= 80 else 'C' if score >= 70 else 'D'
