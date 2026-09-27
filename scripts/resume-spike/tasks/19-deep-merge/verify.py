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
    import copy
    from merge import deep_merge
    base = {"a": 1, "n": {"x": 1, "y": [1], "z": 0}, "l": [1, 2], "gone": 5}
    over = {"b": 2, "n": {"y": [2], "z": None, "w": {"q": 1}}, "l": [3], "gone": None}
    b0, o0 = copy.deepcopy(base), copy.deepcopy(over)
    got = deep_merge(base, over)
    assert got == {"a": 1, "b": 2, "n": {"x": 1, "y": [1, 2], "w": {"q": 1}}, "l": [1, 2, 3]}, got
    assert base == b0 and over == o0
    got["n"]["y"].append(9)
    got["l"].append(9)
    assert base == b0 and over == o0, "the result shares structure with an input"
    assert deep_merge({"a": {"b": 1}}, {"a": 2}) == {"a": 2}
    assert deep_merge({}, {"x": None}) == {}


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
