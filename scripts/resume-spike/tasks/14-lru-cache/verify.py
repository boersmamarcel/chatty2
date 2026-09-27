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
    from lru import LRUCache
    evicted = []
    c = LRUCache(2, on_evict=lambda k, v: evicted.append((k, v)))
    c.put("a", 1)
    c.put("b", 2)
    assert c.get("a") == 1
    c.put("c", 3)
    assert evicted == [("b", 2)], evicted
    assert c.get("b", "none") == "none"
    c.put("a", 10)
    assert evicted == [("b", 2)]
    assert len(c) == 2
    assert "c" in c and "b" not in c
    c.put("d", 4)
    assert evicted == [("b", 2), ("c", 3)], evicted
    assert raises(ValueError, LRUCache, 0)
    plain = LRUCache(1)
    plain.put(1, 1)
    plain.put(2, 2)
    assert len(plain) == 1 and plain.get(2) == 2


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
