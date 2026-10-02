"""A tiny CSV line splitter (no newlines inside fields)."""


def split_line(line):
    """Split one CSV line into fields; quoted fields may hold commas and doubled quotes."""
    fields, field, quoted = [], "", False
    i = 0
    while i < len(line):
        ch = line[i]
        if quoted:
            if ch == '"':
                quoted = False
            else:
                field += ch
        elif ch == '"':
            quoted = True
        elif ch == ",":
            fields.append(field)
            field = ""
        else:
            field += ch
        i += 1
    fields.append(field)
    return fields
