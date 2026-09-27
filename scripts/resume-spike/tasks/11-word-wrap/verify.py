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
    from wrap import wrap
    assert wrap("the quick brown fox", 10) == ["the quick", "brown fox"]
    assert wrap("abcdefghij kl", 4) == ["abcd", "efgh", "ij", "kl"], wrap("abcdefghij kl", 4)
    assert wrap("abcdefg x", 3) == ["abc", "def", "g x"], wrap("abcdefg x", 3)
    assert wrap("", 5) == []
    assert wrap("  a   b  ", 1) == ["a", "b"]
    assert raises(ValueError, wrap, "a", 0)
    for line in wrap("supercalifragilistic is a long word indeed", 6):
        assert len(line) <= 6, line


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
