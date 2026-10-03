"""Recent form: the results of a team's last matches as a string of W, D
and L, e.g. 'WWDLW'.

The matches are taken in date order (oldest first; matches on the same
date in file order); the string lists the last `n` of them, oldest first,
so the most recent result is the last letter.
"""


def result_letter(match, team):
    """'W', 'D' or 'L' for `team` in `match`."""
    winner = match.winner()
    if winner is None:
        return "D"
    return "W" if winner == team else "L"


def form(team, matches, n=5):
    """Form string of `team` over its last `n` matches."""
    if n <= 0:
        return ""
    played = sorted((m for m in matches if m.involves(team)), key=lambda m: m.date)
    recent = played[-n:]
    return "".join(result_letter(m, team) for m in recent)


def form_table(teams, matches, n=5):
    """Dict team -> form string for every team in `teams`."""
    return {team: form(team, matches, n) for team in teams}


def streak(team, matches):
    """Current run, e.g. ('W', 3) for three wins in a row; ('', 0) if none."""
    letters = form(team, matches, len(matches))
    if not letters:
        return ("", 0)
    last = letters[-1]
    count = len(letters) - len(letters.rstrip(last))
    return (last, count)
