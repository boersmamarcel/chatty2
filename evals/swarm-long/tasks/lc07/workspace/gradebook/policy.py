"""Category weights and the drop-lowest rule.

A policy file has one category per line; `#` starts a comment:

    # category   weight   options
    homework     30       drop=1
    quizzes      20       drop=2
    midterm      20
    final        30

Weights are percentages of the course grade and must add up to 100.
`drop=N` removes the N weakest results of the category before its average
is taken (see `drop_lowest`).
"""

import collections


class PolicyError(ValueError):
    """Raised for an invalid policy file or rule."""


class CategoryPolicy(object):
    """Weight and options of one category."""

    def __init__(self, name, weight, drop=0):
        self.name = name
        self.weight = float(weight)
        self.drop = int(drop)

    def __repr__(self):
        return "CategoryPolicy(%r, %r, drop=%r)" % (self.name, self.weight, self.drop)


def _parse_option(text, line_no):
    if "=" not in text:
        raise PolicyError("line %d: bad option %r" % (line_no, text))
    key, _, value = text.partition("=")
    key = key.strip().lower()
    if key != "drop":
        raise PolicyError("line %d: unknown option %r" % (line_no, key))
    try:
        drop = int(value)
    except ValueError:
        raise PolicyError("line %d: drop must be a whole number" % line_no)
    if drop < 0:
        raise PolicyError("line %d: drop must not be negative" % line_no)
    return drop


def parse_policy(text):
    """Parse a policy file into an OrderedDict name -> CategoryPolicy."""
    policies = collections.OrderedDict()
    for line_no, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if len(fields) < 2:
            raise PolicyError("line %d: expected '<category> <weight> [drop=N]'" % line_no)
        name = fields[0].lower()
        if name in policies:
            raise PolicyError("line %d: duplicate category %r" % (line_no, name))
        try:
            weight = float(fields[1])
        except ValueError:
            raise PolicyError("line %d: bad weight %r" % (line_no, fields[1]))
        if weight <= 0:
            raise PolicyError("line %d: weight must be positive" % line_no)
        drop = 0
        for option in fields[2:]:
            drop = _parse_option(option, line_no)
        policies[name] = CategoryPolicy(name, weight, drop)
    check_weights(policies)
    return policies


def check_weights(policies):
    """Raise PolicyError unless the weights add up to 100."""
    total = sum(p.weight for p in policies.values())
    if abs(total - 100.0) > 1e-6:
        raise PolicyError("category weights add up to %g, not 100" % total)


def drop_lowest(entries, n):
    """Remove the `n` weakest results.

    `entries` is a list of (Assessment, points) pairs with numeric points
    (late penalties applied, missing work already counted as 0, excused
    work already left out).
    """
    if n <= 0:
        return list(entries)
    ranked = sorted(entries, key=lambda entry: entry[1])
    return ranked[n:]
