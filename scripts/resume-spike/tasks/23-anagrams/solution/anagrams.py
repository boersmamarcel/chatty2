def group_anagrams(words):
    groups = {}
    for word in words:
        key = "".join(sorted(c for c in word.lower() if c.isalpha()))
        groups.setdefault(key, []).append(word)
    return sorted((sorted(g) for g in groups.values()), key=lambda g: g[0])
