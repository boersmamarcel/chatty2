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
    from matrix import transpose, multiply
    assert transpose([[1, 2, 3]]) == [[1], [2], [3]]
    assert transpose([[1, 2], [3, 4]]) == [[1, 3], [2, 4]]
    assert multiply([[1, 2], [3, 4]], [[5], [6]]) == [[17], [39]]
    assert multiply([[2]], [[3]]) == [[6]]
    assert raises(ValueError, multiply, [[1, 2]], [[1, 2]])
    assert raises(ValueError, multiply, [], [[1]])
    assert raises(ValueError, multiply, [[1]], [])
    assert raises(ValueError, multiply, [[1, 2], [3]], [[1], [2]])
    assert raises(ValueError, multiply, [[1, 2]], [[1], [2, 3]])
    assert raises(ValueError, transpose, [[1, 2], [3]])


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
