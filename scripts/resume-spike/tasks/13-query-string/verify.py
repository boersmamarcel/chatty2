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
    from query import parse_query
    with open("query.py") as f:
        assert "urllib" not in f.read(), "query.py must not use urllib"
    assert parse_query("?a=1&b=2") == {"a": "1", "b": "2"}
    assert parse_query("a=1&a=2&b") == {"a": ["1", "2"], "b": ""}, parse_query("a=1&a=2&b")
    assert parse_query("q=hello+world%21&x%20y=%3D") == {"q": "hello world!", "x y": "="}
    assert parse_query("name=%C3%A9t%C3%A9") == {"name": "\u00e9t\u00e9"}
    assert parse_query("") == {}
    assert parse_query("&&a=1&") == {"a": "1"}
    assert parse_query("k=a=b") == {"k": "a=b"}


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
