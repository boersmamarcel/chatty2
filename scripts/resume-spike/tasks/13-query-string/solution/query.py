def _decode(text):
    out = bytearray()
    i = 0
    while i < len(text):
        c, pair = text[i], text[i + 1:i + 3]
        if c == "%" and len(pair) == 2 and all(h in "0123456789abcdefABCDEF" for h in pair):
            out.append(int(pair, 16))
            i += 3
            continue
        out.extend((" " if c == "+" else c).encode("utf-8"))
        i += 1
    return out.decode("utf-8")


def parse_query(qs):
    """Parse a URL query string into a dict; repeated keys give lists."""
    result = {}
    for piece in qs.lstrip("?").split("&"):
        if not piece:
            continue
        key, _, value = piece.partition("=")
        key, value = _decode(key), _decode(value)
        if key in result:
            if not isinstance(result[key], list):
                result[key] = [result[key]]
            result[key].append(value)
        else:
            result[key] = value
    return result
