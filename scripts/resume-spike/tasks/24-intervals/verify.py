#!/usr/bin/env python3
"""Verifier: judges the worktree in argv[1] after the follow-up. Exit 0 = pass."""
import os
import sys
import traceback

sys.path.insert(0, os.path.abspath(sys.argv[1]))
os.chdir(sys.argv[1])
sys.dont_write_bytecode = True


def raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc:
        return True
    except Exception as other:  # the wrong exception is a failure too
        print("expected %s, got %r" % (exc.__name__, other))
        return False
    return False


def main():
    from intervals import merge_intervals
    data = [[5, 6], [1, 2], [2, 3], [8, 10], [9, 9]]
    before = [list(p) for p in data]
    assert merge_intervals(data) == [[1, 3], [5, 6], [8, 10]], merge_intervals(data)
    assert data == before, "the input was modified"
    assert merge_intervals([]) == []
    assert merge_intervals([[1, 4], [2, 3]]) == [[1, 4]]
    assert raises(ValueError, merge_intervals, [[3, 1]])


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
