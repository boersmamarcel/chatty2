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
    from search import find_first, find_last, count
    xs = [1, 2, 2, 2, 5, 7, 7]
    assert find_first(xs, 2) == 1 and find_last(xs, 2) == 3
    assert find_first(xs, 7) == 5 and find_last(xs, 7) == 6
    assert find_first(xs, 3) == -1 and find_last(xs, 3) == -1
    assert count(xs, 2) == 3 and count(xs, 3) == 0 and count([], 1) == 0
    assert find_last([], 1) == -1


    class Counting(object):
        def __init__(self, n):
            self.n, self.reads = n, 0

        def __len__(self):
            return self.n

        def __getitem__(self, i):
            if not 0 <= i < self.n:
                raise IndexError(i)
            self.reads += 1
            return i // 3


    big = Counting(3000000)
    assert find_first(big, 12345) == 37035
    assert find_last(big, 12345) == 37037
    assert count(big, 12345) == 3
    assert big.reads < 400, big.reads


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
