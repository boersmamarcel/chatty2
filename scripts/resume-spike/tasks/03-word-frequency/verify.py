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
    from words import top_words
    assert top_words("The cat and the hat", 2) == [("the", 2), ("and", 1)], top_words("The cat and the hat", 2)
    assert top_words("b a c b a", 3) == [("a", 2), ("b", 2), ("c", 1)]
    assert top_words("Don't don't stop", 1) == [("don't", 2)]
    assert top_words("one two", 10) == [("one", 1), ("two", 1)]
    assert top_words("", 3) == []


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
