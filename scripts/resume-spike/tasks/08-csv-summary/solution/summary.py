import csv


def summarize(path):
    """Sum, mean and count of the numeric columns of a CSV file."""
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        values = {name: [] for name in reader.fieldnames}
        numeric = {name: True for name in reader.fieldnames}
        for row in reader:
            for name in reader.fieldnames:
                cell = (row.get(name) or "").strip()
                if not cell:
                    continue
                try:
                    values[name].append(float(cell))
                except ValueError:
                    numeric[name] = False
    out = {}
    for name, cells in values.items():
        if numeric[name] and cells:
            out[name] = {"sum": sum(cells), "mean": sum(cells) / len(cells), "count": len(cells)}
    return out
