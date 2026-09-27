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
    from rle import encode, decode
    assert encode("aaabcc") == "3a1b2c"
    assert decode("3a1b2c") == "aaabcc"
    assert encode("a" * 12 + "b") == "12a1b"
    assert decode("12a1b") == "a" * 12 + "b"
    assert encode("") == "" and decode("") == ""
    for s in ("x", "zzzzzzzzzzzzzzzzzzzzzzzzzq", "abAB"):
        assert decode(encode(s)) == s, s
    for bad in ("a", "3", "0a", "3a2", "a3"):
        assert raises(ValueError, decode, bad), bad


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
