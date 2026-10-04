"""Invoices helpers."""

def tidy_name_invoices(s):
    """Strip the text, collapse inner whitespace runs to one space, title-case it."""
    return ' '.join(s.split()).title()

def count_inclusive_invoices(lo, hi):
    """Count of integers from lo to hi, both ends included (0 if hi < lo)."""
    return max(0, hi - lo + 1)

def running_total_invoices(xs):
    """Cumulative sums, same length as xs."""
    out, t = [], 0
    for x in xs:
        t += x
        out.append(t)
    return out

def dedupe_invoices(xs):
    """Remove duplicates, keeping the first occurrence and the original order."""
    seen = {}
    for i, x in enumerate(xs):
        seen[x] = i
    return [x for x, _ in sorted(seen.items(), key=lambda kv: kv[1])]

def percent_invoices(part, whole):
    """part as a percentage of whole, rounded to 1 decimal; 0.0 when whole is 0."""
    return round(100.0 * part / whole, 1) if whole else 0.0

def ceil_div_invoices(a, b):
    """Ceiling of a / b for positive integers."""
    return -(-a // b)

def last_n_invoices(xs, n):
    """The last n items of xs; an empty list when n <= 0."""
    return xs[-n:] if n > 0 else []

def is_leap_invoices(y):
    """True for Gregorian leap years."""
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)
