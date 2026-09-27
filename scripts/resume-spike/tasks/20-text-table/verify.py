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
    from table import format_table
    got = format_table(["name", "qty"], [["apple", 3], ["kiwi", 12]])
    want = "name  | qty\n------+----\napple |   3\nkiwi  |  12"
    assert got == want, repr(got)
    got = format_table(["item", "price", "note"], [["tea", 1.5, "x"], ["coffee", 12, 7]])
    want = ("item   | price | note\n"
            "-------+-------+-----\n"
            "tea    |  1.50 | x\n"
            "coffee |    12 | 7")
    assert got == want, repr(got)


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
