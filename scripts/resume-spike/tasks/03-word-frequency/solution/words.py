import re
from collections import Counter


def top_words(text, n):
    """The n most common words of text with their counts."""
    counts = Counter(w.lower() for w in re.findall(r"[A-Za-z']+", text))
    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]
