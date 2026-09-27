import heapq
import itertools


class TodoList:
    def __init__(self):
        self._heap = []
        self._order = itertools.count()

    def add(self, title, priority):
        heapq.heappush(self._heap, (-priority, next(self._order), title))

    def next(self):
        if not self._heap:
            raise IndexError("empty")
        return heapq.heappop(self._heap)[2]

    def peek(self):
        if not self._heap:
            raise IndexError("empty")
        return self._heap[0][2]

    def __len__(self):
        return len(self._heap)
