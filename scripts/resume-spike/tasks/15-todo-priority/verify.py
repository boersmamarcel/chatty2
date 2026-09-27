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
    from todo import TodoList
    t = TodoList()
    for i in range(8):
        t.add("same-%d" % i, 1)
    t.add("urgent", 5)
    t.add("later", 0)
    assert len(t) == 10
    assert t.peek() == "urgent" and len(t) == 10
    assert t.next() == "urgent"
    assert [t.next() for _ in range(8)] == ["same-%d" % i for i in range(8)]
    assert t.next() == "later"
    assert raises(IndexError, t.next)
    assert raises(IndexError, t.peek)


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
