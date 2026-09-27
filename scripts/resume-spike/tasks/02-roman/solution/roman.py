PAIRS = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
         (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")]
VALUES = {"M": 1000, "D": 500, "C": 100, "L": 50, "X": 10, "V": 5, "I": 1}


def to_roman(n):
    """The Roman numeral for 1 <= n <= 3999."""
    if not 1 <= n <= 3999:
        raise ValueError("out of range: %r" % n)
    out = []
    for value, numeral in PAIRS:
        while n >= value:
            out.append(numeral)
            n -= value
    return "".join(out)


def from_roman(s):
    """The value of a canonical Roman numeral."""
    if not s or any(c not in VALUES for c in s):
        raise ValueError("not a numeral: %r" % s)
    total = 0
    for i, c in enumerate(s):
        value = VALUES[c]
        if i + 1 < len(s) and VALUES[s[i + 1]] > value:
            total -= value
        else:
            total += value
    if not 1 <= total <= 3999 or to_roman(total) != s:
        raise ValueError("not canonical: %r" % s)
    return total
