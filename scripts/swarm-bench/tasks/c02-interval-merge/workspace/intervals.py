"""Merge booking intervals."""


def merge(intervals):
    """Merge overlapping or touching [start, end] intervals; the result is sorted."""
    out = []
    for start, end in intervals:
        if out and start < out[-1][1]:
            out[-1][1] = max(out[-1][1], end)
        else:
            out.append([start, end])
    return [tuple(i) for i in out]
