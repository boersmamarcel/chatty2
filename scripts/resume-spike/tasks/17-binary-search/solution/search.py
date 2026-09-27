def _bound(items, target, right):
    lo, hi = 0, len(items)
    while lo < hi:
        mid = (lo + hi) // 2
        if items[mid] < target or (right and items[mid] == target):
            lo = mid + 1
        else:
            hi = mid
    return lo


def find_first(items, target):
    """Index of the first occurrence of target in sorted items, or -1."""
    i = _bound(items, target, False)
    return i if i < len(items) and items[i] == target else -1


def find_last(items, target):
    """Index of the last occurrence of target in sorted items, or -1."""
    i = _bound(items, target, True) - 1
    return i if i >= 0 and items[i] == target else -1


def count(items, target):
    first = find_first(items, target)
    return 0 if first < 0 else find_last(items, target) - first + 1
