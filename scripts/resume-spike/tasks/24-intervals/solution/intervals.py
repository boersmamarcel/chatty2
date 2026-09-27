def merge_intervals(intervals):
    merged = []
    for start, end in sorted(intervals):
        if start > end:
            raise ValueError("start after end: %r" % [start, end])
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    return merged
