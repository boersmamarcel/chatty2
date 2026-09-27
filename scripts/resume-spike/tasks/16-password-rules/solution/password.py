def validate(password):
    """The names of the rules the password breaks."""
    broken = []
    if len(password) < 12:
        broken.append("length")
    if not any(c.isupper() for c in password):
        broken.append("upper")
    if not any(c.islower() for c in password):
        broken.append("lower")
    if not any(c.isdigit() for c in password):
        broken.append("digit")
    if any(c.isspace() for c in password):
        broken.append("space")
    return broken
