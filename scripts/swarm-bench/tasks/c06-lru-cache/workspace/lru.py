"""A small least-recently-used cache."""

from collections import OrderedDict


class LRUCache(object):
    def __init__(self, capacity):
        self.capacity = capacity
        self.data = OrderedDict()

    def get(self, key, default=None):
        """The value for key, which counts as a use."""
        if key not in self.data:
            return default
        return self.data[key]

    def put(self, key, value):
        """Store key; when full, evict the least recently used key."""
        if key in self.data:
            self.data.move_to_end(key)
        self.data[key] = value
        if len(self.data) > self.capacity:
            self.data.popitem(last=False)
