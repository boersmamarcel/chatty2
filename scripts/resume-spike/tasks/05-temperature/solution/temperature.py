ZERO = {"C": -273.15, "F": -459.67, "K": 0.0}


def _to_c(value, unit):
    if unit == "C":
        return value
    if unit == "F":
        return (value - 32) * 5 / 9
    return value - 273.15


def _from_c(value, unit):
    if unit == "C":
        return value
    if unit == "F":
        return value * 9 / 5 + 32
    return value + 273.15


def convert(value, from_unit, to_unit):
    """Convert a temperature between units."""
    if from_unit not in ZERO or to_unit not in ZERO:
        raise ValueError("unknown unit")
    if value < ZERO[from_unit]:
        raise ValueError("below absolute zero")
    if from_unit == to_unit:
        return value
    return _from_c(_to_c(value, from_unit), to_unit)
