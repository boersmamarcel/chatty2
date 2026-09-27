def format_table(headers, rows):
    numeric = [bool(rows) and all(isinstance(r[i], (int, float)) and not isinstance(r[i], bool)
                                  for r in rows) for i in range(len(headers))]

    def text(value):
        return "%.2f" % value if isinstance(value, float) else str(value)

    cells = [[str(h) for h in headers]] + [[text(v) for v in r] for r in rows]
    widths = [max(len(line[i]) for line in cells) for i in range(len(headers))]

    def line(values):
        parts = [v.rjust(w) if num else v.ljust(w) for v, w, num in zip(values, widths, numeric)]
        return " | ".join(parts).rstrip()

    out = [line(cells[0]), "-+-".join("-" * w for w in widths)]
    out.extend(line(r) for r in cells[1:])
    return "\n".join(out)
