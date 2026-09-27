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
    from password import validate
    assert validate("Abcdefghijk1") == []
    assert validate("Abcdefgh1") == ["length"]
    assert validate("abc def") == ["length", "upper", "digit", "space"], validate("abc def")
    assert validate("ABCDEFGHIJKL1 ") == ["lower", "space"]
    assert validate("abcdefghijkl") == ["upper", "digit"]


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
