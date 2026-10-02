"""Unit conversion to kilograms."""

TO_KG = {"kg": 1.0, "g": 0.01, "t": 1000.0}


def to_kg(amount, unit):
    return amount * TO_KG[unit]
