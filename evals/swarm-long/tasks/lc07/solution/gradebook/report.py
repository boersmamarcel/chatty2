"""Class statistics and the plain-text grade report."""

import math


def mean(values):
    """Arithmetic mean of a non-empty sequence."""
    values = list(values)
    return sum(values) / len(values)


def median(values):
    """Median of a non-empty sequence."""
    ordered = sorted(values)
    middle = len(ordered) // 2
    if len(ordered) % 2 == 0:
        return (ordered[middle - 1] + ordered[middle]) / 2
    return ordered[middle]


def stdev(values):
    """Standard deviation of the class percentages."""
    values = list(values)
    if len(values) < 2:
        return 0.0
    centre = mean(values)
    return math.sqrt(sum((v - centre) ** 2 for v in values) / (len(values) - 1))


def class_stats(percentages):
    """Summary of the course percentages of a class.

    Returns a dict with count, mean, median, stdev, min and max; every value
    except count is rounded to 2 decimals.
    """
    values = list(percentages)
    if not values:
        raise ValueError("no percentages")
    return {
        "count": len(values),
        "mean": round(mean(values), 2),
        "median": round(median(values), 2),
        "stdev": round(stdev(values), 2),
        "min": round(min(values), 2),
        "max": round(max(values), 2),
    }


def rank_rows(rows):
    """Order report rows best first and number them.

    `rows` holds (name, percentage, letter) tuples. Returns
    (rank, name, percentage, letter) tuples sorted by percentage descending,
    then by name.
    """
    ordered = sorted(rows, key=lambda row: (-row[1], row[0]))
    ranked = []
    for index, (name, pct, letter) in enumerate(ordered):
        if index and pct == ordered[index - 1][1]:
            rank = ranked[-1][0]
        else:
            rank = index + 1
        ranked.append((rank, name, pct, letter))
    return ranked


def format_report(rows, title="Grades"):
    """Fixed-width text report of ranked rows plus a statistics footer."""
    ranked = rank_rows(rows)
    width = max([len("Student")] + [len(name) for _, name, _, _ in ranked])
    lines = [title, "=" * len(title), ""]
    lines.append("%4s  %-*s  %7s  %s" % ("Rank", width, "Student", "Percent", "Grade"))
    for rank, name, pct, letter in ranked:
        lines.append("%4d  %-*s  %7.2f  %s" % (rank, width, name, pct, letter))
    if ranked:
        stats = class_stats(pct for _, _, pct, _ in ranked)
        lines.append("")
        lines.append("n=%(count)d  mean=%(mean).2f  median=%(median).2f  sd=%(stdev).2f" % stats)
    return "\n".join(lines)
