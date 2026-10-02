"""Layered configuration."""

DEFAULTS = {"server": {"port": 8080, "hosts": ["localhost"]}, "debug": False}


def deep_merge(base, override):
    """A new dict: override's values on top of base's, merging nested dicts.

    Lists in override replace lists in base. Neither argument is modified.
    """
    result = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(result.get(key), dict):
            result[key] = deep_merge(result[key], value)
        else:
            result[key] = value
    return result


def load(override):
    return deep_merge(DEFAULTS, override)
