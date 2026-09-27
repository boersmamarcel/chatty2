def _shape(m):
    if not m or not m[0]:
        raise ValueError("empty matrix")
    width = len(m[0])
    if any(len(row) != width for row in m):
        raise ValueError("ragged matrix")
    return len(m), width


def transpose(m):
    _shape(m)
    return [list(col) for col in zip(*m)]


def multiply(a, b):
    rows, inner = _shape(a)
    inner_b, cols = _shape(b)
    if inner != inner_b:
        raise ValueError("shape mismatch")
    return [[sum(a[i][k] * b[k][j] for k in range(inner)) for j in range(cols)]
            for i in range(rows)]
