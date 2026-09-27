import re


def slugify(text):
    """Turn a title into a URL slug."""
    text = re.sub(r"\s+", "-", text.lower())
    text = re.sub(r"[^a-z0-9-]", "", text)
    return re.sub(r"-+", "-", text).strip("-")
